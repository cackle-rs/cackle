//! This module handles wrapping and invocation of rustc, the linker and build.rs binaries.
//!
//! We always call through (proxy) to the real rustc and on the happy path, call the real linker.
//!
//! We wrap rustc for the following purposes:
//!
//! * So that we can add -Funsafe-code to all crates that aren't listed in cackle.toml as allowing
//!   unsafe code.
//! * So that we can override the linker with `-C linker=...`
//!
//! We wrap the linker so that:
//!
//! * We can get a list of all the objects and rlibs that are going to be linked and check that the
//!   rules in cackle.toml are satisfied.
//! * We can prevent the actual linker from being invoked if the rules aren't satisfied.
//! * We can put our binary in place of the output for build scripts so that we can proxy them.
//!
//! We wrap build.rs binaries so that:
//!
//! * We can run them inside a sandbox if the config says to do so.
//! * We can capture their output and check for any directives to cargo that haven't been permitted.

use self::rpc::Request;
use crate::config::CommonConfig;
use crate::config::Config;
use crate::crate_index::CrateIndex;
use crate::outcome::ExitCode;
use crate::outcome::Outcome;
use crate::Args;
use crate::RequestHandler;
use anyhow::Context;
use anyhow::Result;
use std::fmt::Display;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) mod cargo;
pub(crate) mod errors;
pub(crate) mod rpc;
pub(crate) mod subprocess;

const SOCKET_ENV: &str = "CACKLE_SOCKET_PATH";
const CONFIG_PATH_ENV: &str = "CACKLE_CONFIG_PATH";
const ORIG_LINKER_ENV: &str = "CACKLE_ORIG_LINKER";

#[derive(Debug)]
pub(crate) struct CargoBuildFailure {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub(crate) struct CargoRunner<'a> {
    pub(crate) manifest_dir: &'a Path,
    pub(crate) tmpdir: &'a Path,
    pub(crate) config: &'a Config,
    pub(crate) args: &'a Args,
    pub(crate) crate_index: &'a CrateIndex,
}

pub(crate) fn clean(dir: &Path, args: &Args, config: &CommonConfig) -> Result<()> {
    // For now, we always clean before we build. It might be possible to not do this, but we'd need
    // to carefully track changes to things we care about, like cackle.toml.
    let mut command = cargo::command("clean", dir, args, config);
    if args.should_capture_cargo_output() {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }
    run_command(&mut command)?;
    Ok(())
}

impl<'a> CargoRunner<'a> {
    /// Invokes `cargo build` in the specified directory with us acting as proxy versions of rustc and
    /// the linker. If calling this, you must call handle_wrapped_binaries from the start of main.
    pub(crate) fn invoke_cargo_build(
        &self,
        abort_recv: Receiver<()>,
        abort_sender: Sender<()>,
        request_creator: impl Fn(Request) -> RequestHandler,
    ) -> Result<()> {
        if !std::env::var(SOCKET_ENV).unwrap_or_default().is_empty() {
            panic!("{SOCKET_ENV} is already set. Missing call to handle_wrapped_binarie?");
        }

        let ipc_path = self.tmpdir.join("cackle.socket");
        let _ = std::fs::remove_file(&ipc_path);
        let listener = UnixListener::bind(&ipc_path)
            .with_context(|| format!("Failed to create Unix socket `{}`", ipc_path.display()))?;

        let mut command =
            cargo::command("build", self.manifest_dir, self.args, &self.config.common);
        let default_build_flags = ["--all-targets".to_owned()];
        for flag in self
            .config
            .common
            .build_flags
            .as_deref()
            .unwrap_or(default_build_flags.as_slice())
        {
            command.arg(flag);
        }
        if let Some(target) = &self.args.target {
            command.arg("--target").arg(target);
        }
        if !self.config.common.features.is_empty() {
            command.arg("--features");
            command.arg(self.config.common.features.join(","));
        }
        let config_path = crate::config::flattened_config_path(self.tmpdir);
        command
            .env(SOCKET_ENV, &ipc_path)
            .env(CONFIG_PATH_ENV, config_path)
            .env("RUSTC_WRAPPER", cackle_exe()?);

        self.crate_index.add_internal_env(&mut command);

        // Don't pass through environment variables that might have been set by `cargo run`. If we do,
        // then they might still be set in our subprocesses, which might then get confused and think
        // they're proxying the build of "cackle" itself.
        command.env_remove("CARGO_PKG_NAME");
        let capture_output = self.args.should_capture_cargo_output();
        if capture_output {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        let mut cargo_process = command
            .spawn()
            .with_context(|| format!("Failed to run {command:?}"))?;

        let mut stdout_thread = None;
        let mut stderr_thread = None;
        if capture_output {
            stdout_thread = Some(start_output_collecting_thread(
                "cargo-stdout-reader",
                cargo_process.stdout.take().unwrap(),
            )?);
            stderr_thread = Some(start_output_collecting_thread(
                "cargo-stderr-reader",
                cargo_process.stderr.take().unwrap(),
            )?);
        }

        listener
            .set_nonblocking(true)
            .context("Failed to set socket to non-blocking")?;
        let (error_send, error_recv) = channel();
        loop {
            if let Some(status) = cargo_process.try_wait()? {
                // The following unwrap will only panic if an output collecting thread panicked.
                let stdout = stdout_thread
                    .take()
                    .map(|thread| thread.join().unwrap())
                    .unwrap_or_default();
                let stderr = stderr_thread
                    .take()
                    .map(|thread| thread.join().unwrap())
                    .unwrap_or_default();
                drop(listener);
                // Deleting the socket is best-effort only, so we don't report an error if we can't.
                let _ = std::fs::remove_file(&ipc_path);
                if let Ok(error) = error_recv.try_recv() {
                    return Err(error);
                }
                if status.code() != Some(0) {
                    return Err(CargoBuildFailure { stdout, stderr }.into());
                }
                break;
            }
            if abort_recv.try_recv().is_ok() {
                let _ = cargo_process.kill();
            }
            // We need to concurrently accept connections from our proxy subprocesses and also check to
            // see if our main subprocess has terminated. It should be possible to do this without
            // polling... but it's so much simpler to just poll.
            if let Ok((mut connection, _)) = listener.accept() {
                let request: rpc::Request = rpc::read_from_stream(&mut connection)
                    .context("Malformed request from subprocess")?;
                let request_handler = (request_creator)(request);
                let error_send = error_send.clone();
                let abort_sender = abort_sender.clone();
                std::thread::Builder::new()
                    .name("Request handler".to_owned())
                    .spawn(move || {
                        if let Err(error) =
                            process_request(request_handler, connection, abort_sender)
                        {
                            let _ = error_send.send(error);
                        }
                    })?;
            } else {
                // Avoid using too much CPU with our polling.
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        Ok(())
    }
}

fn start_output_collecting_thread(
    thread_name: &str,
    mut reader: impl std::io::Read + Send + 'static,
) -> Result<JoinHandle<Vec<u8>>> {
    Ok(std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || -> Vec<u8> {
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            output
        })?)
}

fn process_request(
    mut request_handler: RequestHandler,
    mut connection: UnixStream,
    abort_sender: Sender<()>,
) -> Result<()> {
    let response = request_handler.handle_request();
    let can_continue = response.as_ref().unwrap_or(&Outcome::GiveUp);
    if can_continue == &Outcome::GiveUp {
        // Send an abort signal to cargo, otherwise if we're not capturing the output from cargo,
        // we'll see errors not related to the problem encountered.
        let _ = abort_sender.send(());
    }
    rpc::write_to_stream(&can_continue, &mut connection)?;
    response?;
    Ok(())
}

fn run_command(command: &mut Command) -> Result<ExitCode> {
    Ok(command
        .status()
        .with_context(|| {
            format!(
                "Failed to run `{}`",
                command.get_program().to_string_lossy()
            )
        })?
        .into())
}

fn cackle_exe() -> Result<PathBuf> {
    std::env::current_exe().context("Failed to get current exe")
}

impl Display for CargoBuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.stdout))?;
        write!(f, "{}", String::from_utf8_lossy(&self.stderr))?;
        Ok(())
    }
}

impl std::error::Error for CargoBuildFailure {}
