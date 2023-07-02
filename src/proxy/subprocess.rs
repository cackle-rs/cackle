//! This module contains code that is intended for running in a subprocess whenn we're proxying
//! rustc, the linker or a build script. See comment on parent module for more details.

use super::cackle_exe;
use super::errors::UnsafeUsage;
use super::rpc::BuildScriptOutput;
use super::run_command;
use super::ExitCode;
use super::CONFIG_PATH_ENV;
use crate::config::Config;
use crate::crate_index::CrateIndex;
use crate::link_info::LinkInfo;
use crate::outcome::Outcome;
use crate::proxy::errors::ErrorKind;
use crate::proxy::rpc::RpcClient;
use crate::unsafe_checker;
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
use std::sync::Arc;

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
        exit_status = proxy_rustc(&rpc_client)?;
    } else if let Ok(link_info) = LinkInfo::from_env() {
        // We're wrapping the linker.
        exit_status = proxy_linker(link_info, rpc_client, args)?;
    } else {
        // We're not sure what we're wrapping, something went wrong.
        let args: Vec<String> = std::env::args().collect();
        bail!("Unexpected proxy invocation with args: {args:?}");
    };
    std::process::exit(exit_status.code());
}

/// Renames the binary produced from build.rs and puts our binary in its place. This lets us wrap
/// the build script.
fn setup_build_script_wrapper(build_script_bin: &PathBuf) -> Result<()> {
    std::fs::rename(build_script_bin, orig_build_rs_bin_path(build_script_bin)).with_context(
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

fn proxy_build_script(orig_build_script: PathBuf, rpc_client: &RpcClient) -> Result<ExitCode> {
    loop {
        let config = get_config_from_env()?;
        let package_name = get_env("CARGO_PKG_NAME")?;
        let sandbox_config = config.sandbox_config_for_build_script(&package_name);
        let Some(mut sandbox) = crate::sandbox::from_config(&sandbox_config)? else {
            // Config says to run without a sandbox.
            return Ok(Command::new(&orig_build_script).status()?.into());
        };
        // Allow read access to the crate's root source directory.
        sandbox.ro_bind(Path::new(&get_env("CARGO_MANIFEST_DIR")?));
        sandbox.ro_bind(target_subdir(&orig_build_script)?);
        // Allow write access to OUT_DIR.
        sandbox.writable_bind(Path::new(&get_env("OUT_DIR")?));
        sandbox.pass_cargo_env();

        let output = sandbox.run(&orig_build_script)?;
        let rpc_response = rpc_client.build_script_complete(BuildScriptOutput::new(
            &output,
            package_name,
            &output.status,
            sandbox_config,
            orig_build_script.clone(),
        ))?;
        match rpc_response {
            Outcome::Continue => {
                if output.status.code() == Some(0) {
                    std::io::stderr().lock().write_all(&output.stderr)?;
                    std::io::stdout().lock().write_all(&output.stdout)?;
                    return Ok(output.status.into());
                }
                // If the build script failed and we were asked to proceed, then fall through and
                // retry the build script with a hopefully changed config.
            }
            Outcome::GiveUp => std::process::exit(-1),
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

fn proxy_rustc(rpc_client: &RpcClient) -> Result<ExitCode> {
    loop {
        let mut args = std::env::args().skip(2).peekable();
        let config = get_config_from_env()?;
        let pkg_name =
            std::env::var("CARGO_PKG_NAME").map_err(|_| anyhow!("CARGO_PKG_NAME not set"))?;

        let mut command = Command::new("rustc");
        let mut linker_arg = OsString::new();
        let mut orig_linker_arg = None;
        let is_build_script = std::env::var("CARGO_CRATE_NAME")
            .map(|v| v.starts_with("build_script_"))
            .unwrap_or(false);
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
        let crate_name = if is_build_script {
            format!("{pkg_name}.build")
        } else {
            pkg_name
        };
        let unsafe_permitted = config.unsafe_permitted_for_crate(&crate_name);
        if !unsafe_permitted {
            command.arg("-Funsafe-code");
        }
        let output = command.output()?;
        if output.status.code() == Some(0) {
            if !unsafe_permitted {
                if let Some(unsafe_usage) = find_unsafe_in_sources()? {
                    let response = rpc_client.crate_uses_unsafe(&crate_name, unsafe_usage)?;
                    if response == Outcome::Continue {
                        continue;
                    }
                }
            }
            std::io::stdout().lock().write_all(&output.stdout)?;
            std::io::stderr().lock().write_all(&output.stderr)?;
        } else {
            let crate_name = &crate_name;
            let output = &output;
            let stderr =
                std::str::from_utf8(&output.stderr).context("rustc emitted invalid UTF-8")?;
            match super::errors::get_error(stderr) {
                Some(ErrorKind::Unsafe(usage)) => {
                    let response = rpc_client.crate_uses_unsafe(crate_name, usage)?;
                    if response == Outcome::Continue {
                        continue;
                    }
                }
                _ => {
                    std::io::stdout().lock().write_all(&output.stdout)?;
                    std::io::stderr().lock().write_all(&output.stderr)?;
                }
            }
        }
        return Ok(output.status.into());
    }
}

/// Searches for the first unsafe keyword in the sources for the current invocation of rustc.
fn find_unsafe_in_sources() -> Result<Option<UnsafeUsage>> {
    for file in crate::deps::source_files_from_rustc_args(std::env::args())? {
        if let Some(unsafe_usage) = unsafe_checker::scan_path(&file)? {
            return Ok(Some(unsafe_usage));
        }
    }
    Ok(None)
}

/// Advises our parent process that the linker has been invoked, then once it is done checking the
/// object files, proceeds to run the actual linker or fails.
fn proxy_linker(
    link_info: LinkInfo,
    rpc_client: RpcClient,
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitCode, anyhow::Error> {
    let build_script_bin = link_info
        .is_build_script
        .then(|| link_info.output_file.clone());
    match rpc_client.linker_invoked(link_info)? {
        Outcome::Continue => {
            let exit_status = invoke_real_linker(args)?;
            if exit_status.is_ok() {
                if let Some(build_script_bin) = build_script_bin {
                    setup_build_script_wrapper(&build_script_bin)?;
                }
            }
            Ok(exit_status)
        }
        Outcome::GiveUp => std::process::exit(1),
    }
}

fn invoke_real_linker(
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitCode, anyhow::Error> {
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
    "cc".to_owned()
}

fn get_config_from_env() -> Result<Arc<Config>> {
    let Ok(config_path) = std::env::var(CONFIG_PATH_ENV) else {
        bail!("Internal env var `{}` not set", CONFIG_PATH_ENV);
    };
    // We pass an empty crate index here. That means that the config file we load cannot load config
    // from any other crates. It won't need to though, because the config file we pass will be one
    // that the parent process wrote after it loaded any required crate-specific config files and
    // then flattened them into a single file.
    crate::config::parse_file(Path::new(&config_path), &CrateIndex::default())
}

fn get_env(var_name: &str) -> Result<String> {
    std::env::var(var_name).with_context(|| "Failed to get environment variable `{var_name}`")
}
