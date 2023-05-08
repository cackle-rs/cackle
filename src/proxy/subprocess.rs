//! This module contains code that is intended for running in a subprocess whenn we're proxying
//! rustc, the linker or a build script. See comment on parent module for more details.

use crate::config::Config;
use crate::link_info::LinkInfo;
use crate::proxy::errors::ErrorKind;
use crate::proxy::rpc::RpcClient;
use crate::sandbox::SandboxCommand;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;

use super::cackle_exe;
use super::rpc::BuildScriptOutput;
use super::rpc::CanContinueResponse;
use super::run_command;
use super::CONFIG_PATH_ENV;

/// Checks if we're acting as a wrapper for rustc or the linker. If we are, then we do whatever work
/// we need to do, then invoke the binary that we're wrapping and then exit - i.e. we don't return.
/// If we're not wrapping a binary, then we just return.
pub(crate) fn handle_wrapped_binaries() -> Result<()> {
    let socket_path = std::env::var(super::SOCKET_ENV).unwrap_or_default();
    if socket_path.is_empty() {
        return Ok(());
    }
    let rpc_client = RpcClient::new(socket_path.into());

    let mut args = std::env::args().peekable();
    let binary_name = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing all args"))?);
    let exit_status;
    if let Some(orig_build_script) = proxied_build_rs_bin_path(&binary_name) {
        // We're wrapping a build script.
        exit_status = proxy_build_script(orig_build_script, &rpc_client)?;
    } else if args.peek().map(|arg| arg == "rustc").unwrap_or(false) {
        // We're wrapping rustc.
        args.next();
        exit_status = proxy_rustc(&mut args, &rpc_client)?;
    } else if let Ok(link_info) = LinkInfo::from_env() {
        // We're wrapping the linker.
        exit_status = proxy_linker(link_info, rpc_client, args)?;
    } else {
        // We're not sure what we're wrapping, something went wrong.
        let args: Vec<String> = std::env::args().collect();
        bail!("Unexpected proxy invocation with args: {args:?}");
    };
    std::process::exit(exit_status.code().unwrap_or(-1));
}

/// Renames the binary produced from build.rs and puts our binary in its place. This lets us wrap
/// the build script.
fn setup_build_script_wrapper(build_script_bin: &PathBuf) -> Result<()> {
    std::fs::rename(build_script_bin, orig_build_rs_bin_path(&build_script_bin)).with_context(
        || {
            format!(
                "Failed to rename build.rs binary `{}`",
                build_script_bin.display()
            )
        },
    )?;
    let cackle_exe = cackle_exe()?;
    // Note, we use hard links rather than symbolic links because cargo apparently canonicalises the
    // path to the build script binary when it runs it, so if we give it a symlink, we don't know
    // what we're supposed to be proxying, we just see arg[0] as the path to cackle.
    if std::fs::hard_link(&cackle_exe, build_script_bin).is_err() {
        // If hard linking fails, e.g. because the cackle binary is on a different filesystem to
        // where we're building, then fall back to copying.
        std::fs::copy(&cackle_exe, build_script_bin).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                cackle_exe.display(),
                build_script_bin.display()
            )
        })?;
    }
    Ok(())
}

/// Determines if we're supposed to be running a build script and if we are, returns the path to the
/// actual build script. `binary_path` should be the path via which we were invoked - i.e. arg[0].
fn proxied_build_rs_bin_path(binary_name: &Path) -> Option<PathBuf> {
    let orig = orig_build_rs_bin_path(binary_name);
    if orig.exists() {
        Some(orig)
    } else {
        None
    }
}

/// Returns the name of the actual build.rs file after we've renamed it.
fn orig_build_rs_bin_path(path: &Path) -> PathBuf {
    path.with_file_name("original-build-script")
}

fn proxy_build_script(orig_build_script: PathBuf, rpc_client: &RpcClient) -> Result<ExitStatus> {
    loop {
        let config = get_config_from_env()?;
        let package_name = get_env("CARGO_PKG_NAME")?;
        let sandbox_config = config.sandbox_config_for_build_script(&package_name);
        let Some(mut sandbox_cmd) = SandboxCommand::from_config(&sandbox_config)? else {
            // Config says to run without a sandbox.
            return Ok(Command::new(orig_build_script).status()?);
        };
        // Allow read access to the crate's root source directory.
        sandbox_cmd.ro_bind(get_env("CARGO_MANIFEST_DIR")?);
        sandbox_cmd.ro_bind(target_subdir(&orig_build_script)?);
        // Allow write access to OUT_DIR.
        sandbox_cmd.writable_bind(get_env("OUT_DIR")?);
        sandbox_cmd.pass_cargo_env();

        let output = sandbox_cmd.command_to_run(&orig_build_script).output()?;
        let rpc_response = rpc_client.buid_script_complete(BuildScriptOutput::new(
            &output,
            package_name,
            &output.status,
        ))?;
        match rpc_response {
            CanContinueResponse::Proceed => {
                if output.status.code() == Some(0) {
                    std::io::stderr().lock().write(&output.stderr)?;
                    std::io::stdout().lock().write(&output.stdout)?;
                    return Ok(output.status);
                }
                // If the build script failed and we were asked to proceed, then fall through and
                // retry the build script with a hopefully changed config.
            }
            CanContinueResponse::Deny => std::process::exit(-1),
        }
    }
}

/// Given some path in our target/profile directory, returns the profile directory. This is always
/// "cackle", since we specify what profile to use.
fn target_subdir(build_script_path: &Path) -> Result<&Path> {
    let mut path = build_script_path;
    loop {
        if path.file_name() == Some(OsStr::new(super::cargo::PROFILE_NAME)) {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            path = parent;
        } else {
            bail!(
                "Build script path `{}` expected to be under `{}`",
                build_script_path.display(),
                super::cargo::PROFILE_NAME
            );
        }
    }
}

fn proxy_rustc(
    args: &mut std::iter::Peekable<std::env::Args>,
    rpc_client: &RpcClient,
) -> Result<ExitStatus, anyhow::Error> {
    let config = get_config_from_env()?;
    let crate_name = get_crate_name_from_rustc_args();

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
        command.env(super::ORIG_LINKER_ENV, orig_linker);
    }
    linker_arg.push("linker=");
    linker_arg.push(cackle_exe()?);
    command.arg("--error-format=json");
    command.arg("-C").arg(linker_arg);
    // If something goes wrong, it can be handy to have object files left around to examine.
    command.arg("-C").arg("save-temps");
    command.arg("-Ccodegen-units=1");
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
    match super::errors::get_error(stderr) {
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

/// Advises our parent process that the linker has been invoked, then once it is done checking the
/// object files, proceeds to run the actual linker or fails.
fn proxy_linker(
    link_info: LinkInfo,
    rpc_client: RpcClient,
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitStatus, anyhow::Error> {
    let build_script_bin = link_info
        .is_build_script
        .then(|| link_info.output_file.clone());
    match rpc_client.linker_invoked(link_info)? {
        CanContinueResponse::Proceed => {
            let exit_status = invoke_real_linker(args)?;
            if exit_status.code() == Some(0) {
                if let Some(build_script_bin) = build_script_bin {
                    setup_build_script_wrapper(&build_script_bin)?;
                }
            }
            Ok(exit_status)
        }
        CanContinueResponse::Deny => std::process::exit(1),
    }
}

fn invoke_real_linker(
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitStatus, anyhow::Error> {
    let orig_linker = std::env::var(super::ORIG_LINKER_ENV)
        .ok()
        .unwrap_or_else(default_linker);
    let mut command = Command::new(orig_linker);
    command.args(args);
    run_command(&mut command)
}

/// Returns our best guess as to the default linker.
fn default_linker() -> String {
    // Ideally we'd have a way to ask rustc what linker it wants to use, for now we just guess.
    "clang".to_owned()
}

fn get_config_from_env() -> Result<Config> {
    let Ok(config_path) = std::env::var(CONFIG_PATH_ENV) else {
        bail!("Internal env var `{}` not set", CONFIG_PATH_ENV);
    };
    crate::config::parse_file(Path::new(&config_path))
}

/// Looks for `--crate-name` in the arguments and if found, returns the subsequent argument.
fn get_crate_name_from_rustc_args() -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "--crate-name" {
            return args.next();
        }
    }
    None
}

fn get_env(var_name: &str) -> Result<String> {
    std::env::var(var_name).with_context(|| "Failed to get environment variable `{var_name}`")
}
