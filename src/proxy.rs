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
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use std::io::Write;
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

pub(crate) const SOCKET_ENV: &str = "CACKLE_SOCKET_PATH";
const CONFIG_PATH_ENV: &str = "CACKLE_CONFIG_PATH";
const ORIG_LINKER_ENV: &str = "CACKLE_ORIG_LINKER";
pub(crate) const TARGET_DIR: &str = "CACKLE_TARGET_DIR";
pub(crate) const MANIFEST_DIR: &str = "CACKLE_MANIFEST_DIR";
const RUSTC_PATH: &str = "CACKLE_RUSTC_PATH";

/// Environment variables that we need to allow through to rustc when we run rustc in a sandbox.
pub(crate) const RUSTC_ENV_VARS: &[&str] = &[
    SOCKET_ENV,
    CONFIG_PATH_ENV,
    crate::crate_index::MULTIPLE_VERSION_PKG_NAMES_ENV,
];

pub(crate) struct CargoRunner<'a> {
    pub(crate) manifest_dir: &'a Path,
    pub(crate) tmpdir: &'a Path,
    pub(crate) config: &'a Config,
    pub(crate) args: &'a Args,
    pub(crate) crate_index: &'a CrateIndex,
    pub(crate) target_dir: &'a Path,
}

#[derive(Default)]
pub(crate) struct CargoOutputWaiter {
    stderr_thread: Option<JoinHandle<()>>,
    stdout_thread: Option<JoinHandle<()>>,
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
    /// Invokes `cargo build` in the specified directory with us acting as proxy versions of rustc
    /// and the linker. If calling this, you must call handle_wrapped_binaries from the start of
    /// main.
    pub(crate) fn invoke_cargo_build(
        &self,
        abort_recv: Receiver<()>,
        abort_sender: Sender<()>,
        request_creator: impl Fn(Request) -> RequestHandler,
    ) -> Result<CargoOutputWaiter> {
        if !std::env::var(SOCKET_ENV).unwrap_or_default().is_empty() {
            panic!("{SOCKET_ENV} is already set. Missing call to handle_wrapped_binaries?");
        }

        // We put `cackle.socket` into a directory by itself. This lets our rustc sandbox have write
        // permission on this directory without also gaining write access to other files that we put
        // in our temporary directory.
        let ipc_dir = self.tmpdir.join("comms");
        std::fs::create_dir_all(&ipc_dir)
            .with_context(|| format!("Failed to crate directory `{}`", ipc_dir.display()))?;
        let ipc_path = ipc_dir.join("cackle.socket");
        let _ = std::fs::remove_file(&ipc_path);
        let listener = UnixListener::bind(&ipc_path)
            .with_context(|| format!("Failed to create Unix socket `{}`", ipc_path.display()))?;

        let mut command = cargo::command(
            "build",
            self.manifest_dir,
            self.args,
            &self.config.raw.common,
        );
        if self.args.command.is_none() {
            let default_build_flags = ["--all-targets".to_owned()];
            for flag in self
                .config
                .raw
                .common
                .build_flags
                .as_deref()
                .unwrap_or(default_build_flags.as_slice())
            {
                command.arg(flag);
            }
        }
        let rustc_path = rustup_rustc_path().unwrap_or_else(|_| PathBuf::from("rustc"));
        if let Some(target) = &self.args.target {
            command.arg("--target").arg(target);
        }
        let features = self
            .args
            .features
            .clone()
            .unwrap_or_else(|| self.config.raw.common.features.join(","));
        if !features.is_empty() {
            command.arg("--features");
            command.arg(features);
        }
        let config_path = crate::config::flattened_config_path(self.tmpdir);
        command
            .env(SOCKET_ENV, &ipc_path)
            .env(CONFIG_PATH_ENV, config_path)
            .env(TARGET_DIR, self.target_dir)
            .env(MANIFEST_DIR, self.manifest_dir)
            .env(RUSTC_PATH, rustc_path)
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

        let mut output_waiter = CargoOutputWaiter::default();
        if capture_output {
            output_waiter.stdout_thread = Some(start_output_pass_through_thread(
                "cargo-stdout-pass-through",
                cargo_process.stdout.take().unwrap(),
            )?);
            output_waiter.stderr_thread = Some(start_output_pass_through_thread(
                "cargo-stderr-pass-through",
                cargo_process.stderr.take().unwrap(),
            )?);
        }

        listener
            .set_nonblocking(true)
            .context("Failed to set socket to non-blocking")?;
        let (error_send, error_recv) = channel();
        loop {
            if let Some(status) = cargo_process.try_wait()? {
                drop(listener);
                // Deleting the socket is best-effort only, so we don't report an error if we can't.
                let _ = std::fs::remove_file(&ipc_path);
                if let Ok(error) = error_recv.try_recv() {
                    return Err(error);
                }
                if status.code() != Some(0) {
                    bail!("`cargo` exited with non-zero exit status");
                }
                break;
            }
            if abort_recv.try_recv().is_ok() {
                log::info!("Killing cargo process");
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

        Ok(output_waiter)
    }
}

/// Returns the path to rustc as provided by rustup. If rustup is available, then we bypass it when
/// running rustc, since rustup sometimes (at least in CI) seems to write to ~/.rustup, which our
/// sandbox configuration doesn't allow. We don't want to allow write access to ~/.rustup because
/// that would also mean that proc macros could write there.
fn rustup_rustc_path() -> Result<PathBuf> {
    // Note, the call of this function discards errors and just falls back to "rustc".
    let output = Command::new("rustup").arg("which").arg("rustc").output()?;
    if !output.status.success() {
        bail!("rustup which rustc failed");
    }
    let path = PathBuf::from(std::str::from_utf8(&output.stdout)?.trim());
    if !path.exists() {
        panic!(
            "rustup which rustc returned non-existent path: {}",
            path.display()
        );
    }
    Ok(path)
}

impl CargoOutputWaiter {
    /// Wait for all output to pass through and the output threads (if any) to shut down. This
    /// should only be called after the UI thread has been shut down since the output threads block
    /// while the UI is active.
    pub(crate) fn wait_for_output(&mut self) {
        // The following unwraps will only panic if an output-collecting thread panicked.
        if let Some(thread) = self.stdout_thread.take() {
            thread.join().unwrap()
        }
        if let Some(thread) = self.stderr_thread.take() {
            thread.join().unwrap()
        }
    }
}

fn start_output_pass_through_thread(
    thread_name: &str,
    mut reader: impl std::io::Read + Send + 'static,
) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let mut output = vec![0u8; 64];
            while let Ok(size) = reader.read(&mut output) {
                if size == 0 {
                    break;
                }
                // For now, we just send all output to stderr regardless of whether it was
                // originally on stdout or stderr. We lock stderr when the UI (full_term.rs) is
                // active, so the following lock will block the whole time the UI is active. Once
                // the UI shuts down, we'll unblock and send any remaining output through.
                let _ = std::io::stderr().lock().write_all(&output[..size]);
            }
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
