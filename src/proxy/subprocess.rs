//! This module contains code that is intended for running in a subprocess whenn we're proxying
//! rustc, the linker or a build script. See comment on parent module for more details.

use super::cackle_exe;
use super::errors::get_disallowed_unsafe_locations;
use super::rpc::BinExecutionOutput;
use super::rpc::RustcOutput;
use super::run_command;
use super::ExitCode;
use super::CONFIG_PATH_ENV;
use crate::config::Config;
use crate::config::CrateName;
use crate::crate_index::CrateIndex;
use crate::crate_index::CrateKind;
use crate::crate_index::CrateSel;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::outcome::Outcome;
use crate::proxy::rpc::RpcClient;
use crate::unsafe_checker;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

pub(crate) const PROXY_BIN_ARG: &str = "proxy-bin";
pub(crate) const ENV_CRATE_KIND: &str = "CACKLE_CRATE_KIND";

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
    // Skip binary name.
    args.next();
    let exit_status;
    if args.peek().map(|a| a == PROXY_BIN_ARG).unwrap_or(false) {
        // We're wrapping a binary.
        args.next();
        let (Some(selector_token), Some(orig_bin)) = (args.next(), args.next()) else {
            bail!("Missing proxy-bin args");
        };
        let crate_sel = CrateSel::from_env()?.with_selector_token(&selector_token)?;
        exit_status = proxy_binary(PathBuf::from(orig_bin), &crate_sel, &rpc_client)?;
    } else if is_path_to_rustc(args.peek()) {
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

fn is_path_to_rustc(arg: Option<&String>) -> bool {
    arg.and_then(|arg| Path::new(arg).file_name())
        .map(|file_name| file_name == "rustc")
        .unwrap_or(false)
}

/// Renames an output binary and puts our binary in its place. This lets us wrap the binary when it
/// gets executed.
fn setup_bin_wrapper(link_info: &mut LinkInfo) -> Result<()> {
    fn utf8(input: &Path) -> Result<&str> {
        input
            .to_str()
            .ok_or_else(|| anyhow!("Path `{}` is not valid UTF-8", input.display()))
    }

    let bin_path = &link_info.output_file;
    let new_filename = orig_bin_path(bin_path);
    // We could rename the file here, however copying avoids the need to chmod `bin_path` after we
    // write it below.
    std::fs::copy(bin_path, &new_filename)
        .with_context(|| format!("Failed to rename binary `{}`", bin_path.display()))?;
    let cackle_exe = cackle_exe()?;
    let cackle_exe = utf8(&cackle_exe)?;
    let selector_token = link_info.crate_sel.selector_token();
    let bin_path_utf8 = utf8(&new_filename)?;
    // Write a shell script that checks if the cackle binary exists and if it does, executes it so
    // that cackle can run the actual binary, possibly in a sandbox. If the script detects that the
    // cackle binary doesn't exist then likely we're already running in a sandbox due to some other
    // binary having been invoked. In that case, just run the binary directly. This does mean that
    // if one binary is invoked from another, the permissions granted will be those of the outer
    // binary - but the outer binary was what decided to invoke the inner binary, so that seems
    // fair.
    std::fs::write(
        bin_path,
        format!(
            "#!/bin/bash\n\
             if [ -x \"{cackle_exe}\" ]; then\n\
                \"{cackle_exe}\" {PROXY_BIN_ARG} {selector_token} \"{bin_path_utf8}\" \"$@\" \n\
             else\n\
                \"{bin_path_utf8}\" \"$@\"\n\
             fi\n",
        ),
    )?;
    link_info.output_file = new_filename;
    Ok(())
}

/// Returns the name of the real bin file after we've renamed it.
fn orig_bin_path(path: &Path) -> PathBuf {
    if let Some(extension) = path.extension() {
        let mut new_extension = OsString::from("orig.");
        new_extension.push(extension);
        path.with_extension(new_extension)
    } else {
        path.with_extension("orig")
    }
}

fn proxy_binary(
    orig_bin: PathBuf,
    crate_sel: &CrateSel,
    rpc_client: &RpcClient,
) -> Result<ExitCode> {
    loop {
        let config = get_config_from_env()?;
        let crate_name = CrateName::from(crate_sel);
        let sandbox_config = config.sandbox_config_for_package(&crate_name);
        let Some(sandbox) = crate::sandbox::from_config(&sandbox_config, &orig_bin, &crate_name)?
        else {
            // Config says to run without a sandbox.
            return Ok(Command::new(&orig_bin).status()?.into());
        };

        let output = sandbox.run(&orig_bin)?;
        let rpc_response = rpc_client.build_script_complete({
            let exit_code = output.status.code().unwrap_or(-1);
            BinExecutionOutput {
                exit_code,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                crate_sel: crate_sel.clone(),
                sandbox_config,
                build_script: orig_bin.clone(),
                sandbox_config_display: (exit_code != 0)
                    .then(|| sandbox.display_to_run(&orig_bin).to_string()),
            }
        })?;
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

fn proxy_rustc(rpc_client: &RpcClient) -> Result<ExitCode> {
    if std::env::var("CARGO_PKG_NAME").is_err() {
        // If CARGO_PKG_NAME isn't set, then cargo is probably just invoking rustc to query
        // version information etc, just run it.
        return Ok(Command::new("rustc")
            .args(std::env::args().skip(2))
            .status()?
            .into());
    };
    let mut crate_sel = CrateSel::from_env()?;
    if std::env::args().any(|arg| arg == "--test") {
        crate_sel.kind = CrateKind::Test;
    }
    let mut runner = RustcRunner::new(crate_sel)?;
    rpc_client.rustc_started(&runner.crate_sel)?;
    loop {
        match runner.run(rpc_client)? {
            RustcRunStatus::Retry => {}
            RustcRunStatus::GiveUp => return Ok(crate::outcome::FAILURE),
            RustcRunStatus::Done(output) => {
                std::io::stdout().lock().write_all(&output.stdout)?;
                std::io::stderr().lock().write_all(&output.stderr)?;
                return Ok(output.status.into());
            }
        }
    }
}

struct RustcRunner {
    crate_sel: CrateSel,
    linking_requested: bool,
    /// The paths of the sources for the crate being compiled. This is obtained by parsing the deps
    /// file written by rustc the first time we run it.
    source_paths: Option<Vec<PathBuf>>,
}

enum RustcRunStatus {
    Retry,
    GiveUp,
    Done(std::process::Output),
}

impl RustcRunner {
    fn new(crate_sel: CrateSel) -> Result<Self> {
        let linking_requested = Self::linking_requested();
        Ok(Self {
            crate_sel,
            linking_requested,
            source_paths: None,
        })
    }

    fn run(&mut self, rpc_client: &RpcClient) -> Result<RustcRunStatus> {
        // Until source_paths has been filled, we don't allow linking, since linking requires that
        // we have already given this information to the parent cackle process.
        let allow_linking = self.source_paths.is_some();
        // We need to parse the configuration each time, since it might have changed. Specifically
        // it might have been changed to allow unsafe.
        let config = get_config_from_env()?;
        let unsafe_permitted = config.unsafe_permitted_for_crate(&self.crate_sel);
        let mut command = self.get_command(allow_linking, unsafe_permitted)?;
        let output = command.output()?;
        let mut unsafe_locations = Vec::new();

        if output.status.code() == Some(0) {
            if !allow_linking {
                let source_paths = crate::deps::source_files_from_rustc_args(std::env::args())?;
                // Tell the main process what source paths this rustc invocation made use of. It needs
                // these so that it can attribute source files to a particular crate. We ignore the
                // response, since we don't need it.
                rpc_client.rustc_complete(RustcOutput {
                    crate_sel: self.crate_sel.clone(),
                    source_paths: source_paths.clone(),
                })?;
                self.source_paths = Some(source_paths);
                if self.linking_requested {
                    // Retry with linking allowed.
                    return Ok(RustcRunStatus::Retry);
                }
            }
        } else {
            unsafe_locations.extend(get_disallowed_unsafe_locations(&output)?);
        }
        if !unsafe_permitted {
            unsafe_locations.extend(find_unsafe_in_sources(
                self.source_paths.as_deref().unwrap_or_default(),
            )?);
        }
        if !unsafe_locations.is_empty() {
            unsafe_locations.sort();
            unsafe_locations.dedup();
            let response = rpc_client.crate_uses_unsafe(&self.crate_sel, unsafe_locations)?;
            if response == Outcome::Continue {
                return Ok(RustcRunStatus::Retry);
            } else {
                return Ok(RustcRunStatus::GiveUp);
            }
        }

        Ok(RustcRunStatus::Done(output))
    }

    /// Returns whether rustc was asked to link as indicated by --emit=*,link,*.
    fn linking_requested() -> bool {
        std::env::args().any(|arg| {
            arg.strip_prefix("--emit=")
                .map(|emit| emit.split(',').any(|p| p == "link"))
                .unwrap_or(false)
        })
    }

    fn get_command(&self, allow_linking: bool, unsafe_permitted: bool) -> Result<Command> {
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
                // Skip -C debuginfo= if present, so that we can add our own value at the end.
                if args
                    .peek()
                    .map(|arg| arg.starts_with("debuginfo="))
                    .unwrap_or(false)
                {
                    args.next();
                    continue;
                }
            }
            if arg.starts_with("--error-format") {
                continue;
            }
            if let Some(emit) = arg.strip_prefix("--emit=") {
                if self.linking_requested && !allow_linking {
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
        // Force-enable -C debuginfo=2. We need debug info in order to know where code originated.
        command.arg("-C").arg("debuginfo=2");
        if let Some(orig_linker) = orig_linker_arg {
            command.env(super::ORIG_LINKER_ENV, orig_linker);
        }
        linker_arg.push("linker=");
        linker_arg.push(cackle_exe()?);
        command.arg("--error-format=json");
        command.arg("-C").arg(linker_arg);
        command.arg("-C").arg("save-temps");
        command.arg("-Ccodegen-units=1");
        command.env(ENV_CRATE_KIND, self.crate_sel.selector_token());
        if !unsafe_permitted {
            command.arg("-Funsafe-code");
        }
        Ok(command)
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
    mut link_info: LinkInfo,
    rpc_client: RpcClient,
    args: std::iter::Peekable<std::env::Args>,
) -> Result<ExitCode, anyhow::Error> {
    // Invoke the actual linker first, since the parent process uses the output file to aid with
    // analysis.
    let exit_status = invoke_real_linker(args)?;
    if exit_status.is_ok() && link_info.is_executable() {
        setup_bin_wrapper(&mut link_info)?;
    }
    match rpc_client.linker_invoked(link_info)? {
        Outcome::Continue => Ok(exit_status),
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

#[test]
fn test_orig_bin_path() {
    assert_eq!(orig_bin_path(Path::new("foo")), Path::new("foo.orig"));
    assert_eq!(
        orig_bin_path(Path::new("foo.exe")),
        Path::new("foo.orig.exe")
    );
}
