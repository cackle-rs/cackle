//! This module contains code that is intended for running in a subprocess whenn we're proxying
//! rustc, the linker or a build script. See comment on parent module for more details.

use super::cackle_exe;
use super::errors::get_disallowed_unsafe_locations;
use super::rpc::BuildScriptOutput;
use super::rpc::RustcOutput;
use super::run_command;
use super::ExitCode;
use super::CONFIG_PATH_ENV;
use crate::checker::SourceLocation;
use crate::config::Config;
use crate::config::CrateName;
use crate::crate_index::CrateIndex;
use crate::link_info::LinkInfo;
use crate::outcome::Outcome;
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
            CrateName::for_build_script(&package_name),
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
    // When rustc calls the linker (us), we notify the main cackle process, which checks what APIs
    // are used. The main process needs to know which source paths belong to which crates. For it to
    // know this mapping, we need to parse the deps file that gets created when rustc runs. So on
    // the first iteration, we don't allow linking and only emit dep-info. When that completes, we
    // provide the source to crate mapping to the parent process, then repeat compilation, but this
    // time allow linking.
    let mut allow_linking = false;
    let mut first = true;
    loop {
        let Ok(pkg_name) = std::env::var("CARGO_PKG_NAME") else {
            // If CARGO_PKG_NAME isn't set, then cargo is probably just invoking rustc to query
            // version information etc, just run it.
            return Ok(Command::new("rustc")
                .args(std::env::args().skip(2))
                .status()?
                .into());
        };
        let runner = RustcRunner::new(pkg_name)?;
        if first {
            rpc_client.rustc_started(&runner.crate_name)?;
            first = false;
        }
        match runner.run(rpc_client, allow_linking)? {
            RustcRunStatus::Retry => {}
            RustcRunStatus::RetryWithLinkingPermitted => {
                allow_linking = true;
            }
            RustcRunStatus::Done(output) => {
                std::io::stdout().lock().write_all(&output.stdout)?;
                std::io::stderr().lock().write_all(&output.stderr)?;
                return Ok(output.status.into());
            }
        }
    }
}

struct RustcRunner {
    crate_name: CrateName,
    unsafe_permitted: bool,
}

enum RustcRunStatus {
    Retry,
    RetryWithLinkingPermitted,
    Done(std::process::Output),
}

impl RustcRunner {
    fn new(pkg_name: String) -> Result<Self> {
        let config = get_config_from_env()?;
        let is_build_script = std::env::var("CARGO_CRATE_NAME")
            .map(|v| v.starts_with("build_script_"))
            .unwrap_or(false);
        let crate_name = if is_build_script {
            CrateName::for_build_script(&pkg_name)
        } else {
            CrateName::from(pkg_name.as_str())
        };
        let unsafe_permitted = config.unsafe_permitted_for_crate(&crate_name);
        Ok(Self {
            unsafe_permitted,
            crate_name,
        })
    }

    fn run(&self, rpc_client: &RpcClient, allow_linking: bool) -> Result<RustcRunStatus> {
        let (linking_requested, mut command) = self.get_command(allow_linking)?;
        let output = command.output()?;

        if output.status.code() == Some(0) {
            // We only figure out source paths, check for unsafe and notify the parent process of
            // source paths on the first successful run. Doing it a second time would be a waste.
            if !allow_linking {
                let source_paths = crate::deps::source_files_from_rustc_args(std::env::args())?;
                if !self.unsafe_permitted {
                    let mut locations = find_unsafe_in_sources(&source_paths)?;
                    if !locations.is_empty() {
                        // If we're reporting unsafe from token-scanning, then get any additional
                        // unsafe usages from the compiler and merge them in. Otherwise we'd only
                        // report some of the unsafe usages, which would provide an incomplete
                        // picture.
                        if !allow_linking && linking_requested {
                            let (_, mut command_for_linking) = self.get_command(true)?;
                            let mut extra_unsafe_locations =
                                get_disallowed_unsafe_locations(&command_for_linking.output()?)?;
                            locations.append(&mut extra_unsafe_locations);
                            locations.sort();
                            locations.dedup();
                        }
                        let response = rpc_client.crate_uses_unsafe(&self.crate_name, locations)?;
                        if response == Outcome::Continue {
                            return Ok(RustcRunStatus::Retry);
                        }
                    }
                }
                // Tell the main process what source paths this rustc invocation made use of. It needs
                // these so that it can attribute source files to a particular crate. We ignore the
                // response, since we don't need it.
                rpc_client.rustc_complete(RustcOutput {
                    crate_name: self.crate_name.clone(),
                    source_paths,
                })?;
            }
        } else {
            let unsafe_locations = get_disallowed_unsafe_locations(&output)?;
            if !unsafe_locations.is_empty() {
                let response = rpc_client.crate_uses_unsafe(&self.crate_name, unsafe_locations)?;
                if response == Outcome::Continue {
                    return Ok(RustcRunStatus::Retry);
                }
            }
        }
        if linking_requested && !allow_linking {
            return Ok(RustcRunStatus::RetryWithLinkingPermitted);
        }

        Ok(RustcRunStatus::Done(output))
    }

    fn get_command(&self, allow_linking: bool) -> Result<(bool, Command)> {
        let mut linking_requested = false;
        let mut args = std::env::args().skip(2).peekable();
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
            if let Some(emit) = arg.strip_prefix("--emit=") {
                linking_requested = emit.split(',').any(|p| p == "link");
                if linking_requested && !allow_linking {
                    command.arg(format!(
                        "--emit={}",
                        emit.split(',')
                            .filter(|p| *p != "link")
                            .collect::<Vec<_>>()
                            .join(",")
                    ));
                    continue;
                }
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
        command.arg("-C").arg("save-temps");
        command.arg("-Ccodegen-units=1");
        if !self.unsafe_permitted {
            command.arg("-Funsafe-code");
        }
        Ok((linking_requested, command))
    }
}

/// Searches for the unsafe keyword in the specified paths.
fn find_unsafe_in_sources(paths: &[PathBuf]) -> Result<Vec<SourceLocation>> {
    let mut locations = Vec::new();
    for file in paths {
        locations.append(&mut unsafe_checker::scan_path(file)?);
    }
    Ok(locations)
}

/// Runs the real linker, then advises our parent process of all input files to the linker as well
/// as the output file. If the parent process says that all checks have been satisfied, then we
/// return, otherwise we exit.
fn proxy_linker(
    link_info: LinkInfo,
    rpc_client: RpcClient,
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitCode, anyhow::Error> {
    // Invoke the actual linker first, since the parent process uses the output file to aid with
    // analysis.
    let exit_status = invoke_real_linker(args)?;
    let build_script_bin = link_info
        .is_build_script
        .then(|| link_info.output_file.clone());
    match rpc_client.linker_invoked(link_info)? {
        Outcome::Continue => {
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
