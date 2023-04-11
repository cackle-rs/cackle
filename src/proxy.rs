//! This module handles wrapping and invocation of both rustc and the linker.
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

use self::errors::ErrorKind;
use self::rpc::RpcClient;
use crate::config::Config;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::Command;
use std::process::ExitStatus;
use std::thread::JoinHandle;
use std::time::Duration;

mod cargo;
mod errors;
pub(crate) mod rpc;

const SOCKET_ENV: &str = "CACKLE_SOCKET_PATH";
const CONFIG_PATH_ENV: &str = "CACKLE_CONFIG_PATH";
const ORIG_LINKER_ENV: &str = "CACKLE_ORIG_LINKER";

/// Invokes `cargo build` in the specified directory with us acting as proxy versions of rustc and
/// the linker. If calling this, you must call handle_wrapped_binaries from the start of main.
pub(crate) fn invoke_cargo_build(
    dir: &Path,
    config_path: &Path,
    mut callback: impl FnMut(rpc::Request) -> rpc::CanContinueResponse,
) -> Result<()> {
    if !std::env::var(SOCKET_ENV).unwrap_or_default().is_empty() {
        panic!("{SOCKET_ENV} is already set. Missing call to handle_wrapped_binarie?");
    }
    let _ = std::fs::remove_file("/tmp/cackle.log");
    // For now, we always clean before we build. It might be possible to not do this, but we'd need
    // to carefully track changes to things we care about, like cackle.toml.
    run_command(&mut cargo::command("clean", dir))?;

    let target_dir = dir.join("target");
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("Failed to create directory `{}`", target_dir.display()))?;
    let ipc_path = target_dir.join("cackle.socket");
    let listener = UnixListener::bind(&ipc_path)
        .with_context(|| format!("Failed to create Unix socket `{}`", ipc_path.display()))?;

    let mut command = cargo::command("build", dir);
    command
        .env(SOCKET_ENV, &ipc_path)
        .env(CONFIG_PATH_ENV, config_path)
        .env("RUSTC_WRAPPER", cackle_exe()?);

    let cargo_thread: JoinHandle<Result<process::Output>> =
        std::thread::spawn(move || -> Result<process::Output> {
            let output = command
                .output()
                .with_context(|| format!("Failed to run {command:?}"))?;
            Ok(output)
        });

    listener
        .set_nonblocking(true)
        .context("Failed to set socket to non-blocking")?;
    loop {
        if cargo_thread.is_finished() {
            // The following unwrap will only panic if the cargo thread panicked.
            let output = cargo_thread.join().unwrap()?;
            drop(listener);
            // Deleting the socket is best-effort only, so we don't report an error if we can't.
            let _ = std::fs::remove_file(&ipc_path);
            if output.status.code() != Some(0) {
                bail!("cargo build exited with non-zero exit status");
            }
            break;
        }
        // We need to concurrently accept connections from our proxy subprocesses and also check to
        // see if our main subprocess has terminated. It should be possible to do this without
        // polling... but it's so much simpler to just poll.
        if let Ok((mut connection, _)) = listener.accept() {
            let request: rpc::Request = rpc::read_from_stream(&mut connection)
                .context("Malformed request from subprocess")?;
            let response = (callback)(request);
            rpc::write_to_stream(&response, &mut connection)?;
        } else {
            // Avoid using too much CPU with our polling.
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    Ok(())
}

/// Checks if we're acting as a wrapper for rustc or the linker. If we are, then we do whatever work
/// we need to do, then invoke the binary that we're wrapping and then exit - i.e. we don't return.
/// If we're not wrapping a binary, then we just return.
pub(crate) fn handle_wrapped_binaries() -> Result<()> {
    let socket_path = std::env::var(SOCKET_ENV).unwrap_or_default();
    if socket_path.is_empty() {
        return Ok(());
    }
    let rpc_client = RpcClient::new(socket_path.into());

    let mut args = std::env::args().peekable();
    args.next();
    let exit_status = if args.peek().map(|arg| arg == "rustc").unwrap_or(false) {
        args.next();
        // We're wrapping rustc, call the real rustc.
        proxy_rustc(&mut args, &rpc_client)?
    } else {
        // If we're not proxying rustc, then we assume we're proxying the linker. If we ever need to
        // proxy anything else, we might need to look at the arguments to identify what we're doing.
        match rpc_client.linker_args(std::env::args().skip(1).collect())? {
            rpc::CanContinueResponse::Proceed => proxy_linker(args)?,
            rpc::CanContinueResponse::Deny => std::process::exit(0),
        }
    };
    std::process::exit(exit_status.code().unwrap_or(-1));
}

fn proxy_rustc(
    args: &mut std::iter::Peekable<std::env::Args>,
    rpc_client: &RpcClient,
) -> Result<ExitStatus, anyhow::Error> {
    let config = get_config_from_env()?;
    let crate_name = get_crate_name_from_args();

    let mut command = Command::new("rustc");
    let mut linker_arg = OsString::new();
    let mut orig_linker_arg = None;
    while let Some(arg) = args.next() {
        // Look for `-C linker=...`. If we find it, note the value for later use and drop the
        // argument.
        if arg == "-C" {
            if let Some(linker) = args
                .peek()
                .and_then(|arg| arg.strip_prefix("linker="))
                .map(ToOwned::to_owned)
            {
                orig_linker_arg = Some(linker);
                args.next();
                continue;
            }
        }
        if arg.starts_with("--error-format") {
            continue;
        }
        // For all other arguments, pass them through.
        command.arg(arg);
    }
    if let Some(orig_linker) = orig_linker_arg {
        command.env(ORIG_LINKER_ENV, orig_linker);
    }
    linker_arg.push("linker=");
    linker_arg.push(cackle_exe()?);
    command.arg("--error-format=json");
    command.arg("-C").arg(linker_arg);
    // If something goes wrong, it can be handy to have object files left around to examine.
    command.arg("-C").arg("save-temps");
    if let Some(crate_name) = &crate_name {
        if !config.unsafe_permitted_for_crate(crate_name) {
            command.arg("-Funsafe-code");
        }
    }
    let output = command.output()?;
    if output.status.code() == Some(0) {
        std::io::stdout().lock().write_all(&output.stdout)?;
        std::io::stderr().lock().write_all(&output.stderr)?;
    } else {
        handle_rustc_errors(rpc_client, &crate_name, &output)?;
    }
    Ok(output.status)
}

fn handle_rustc_errors(
    rpc_client: &RpcClient,
    crate_name: &Option<String>,
    output: &std::process::Output,
) -> Result<()> {
    let stderr = std::str::from_utf8(&output.stderr).context("rustc emitted invalid UTF-8")?;
    match errors::get_error(stderr) {
        Some(ErrorKind::Unsafe(usage)) => {
            // TODO: Check if the response was to allow the unsafe - in that case rerun with the
            // config altered.
            let _response = rpc_client
                .crate_uses_unsafe(crate_name.as_ref().map(|s| s.as_str()).unwrap_or(""), usage)?;
        }
        _ => {
            std::io::stdout().lock().write_all(&output.stdout)?;
            std::io::stderr().lock().write_all(&output.stderr)?;
        }
    }
    Ok(())
}

fn proxy_linker(args: std::iter::Peekable<std::env::Args>) -> Result<ExitStatus, anyhow::Error> {
    let orig_linker = std::env::var(ORIG_LINKER_ENV)
        .ok()
        .unwrap_or_else(default_linker);
    //let mut command = Command::new(orig_linker);
    let mut command = Command::new("strace");
    command
        .arg("-f")
        .arg("-o")
        .arg("/tmp/l.strace")
        .arg("--string-limit=1000")
        .arg(orig_linker);
    command.args(args);
    run_command(&mut command)
}

/// Returns our best guess as to the default linker.
fn default_linker() -> String {
    // Ideally we'd have a way to ask rustc what linker it wants to use, for now we just guess.
    "clang".to_owned()
}

fn cackle_exe() -> Result<PathBuf> {
    std::env::current_exe().context("Failed to get current exe")
}

fn run_command(command: &mut Command) -> Result<std::process::ExitStatus> {
    command
        .status()
        .with_context(|| format!("Failed to run {command:?}"))
}

fn get_config_from_env() -> Result<Config> {
    let Ok(config_path) = std::env::var(CONFIG_PATH_ENV) else {
        bail!("Internal env var `{}` not set", CONFIG_PATH_ENV);
    };
    crate::config::parse_file(Path::new(&config_path))
}

/// Looks for `--crate-name` in the arguments and if found, returns the subsequent argument.
fn get_crate_name_from_args() -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--crate-name" {
            return args.next();
        }
    }
    None
}
