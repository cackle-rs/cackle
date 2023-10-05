use crate::config::CommonConfig;
use crate::Args;
use clap::Parser;
use std::path::Path;
use std::process::Command;

/// The name of the default cargo profile that we use.
pub(crate) const DEFAULT_PROFILE_NAME: &str = "cackle";
pub(crate) const PROFILE_NAME_ENV: &str = "CACKLE_BUILD_PROFILE";

#[derive(Parser, Debug, Clone)]
pub(crate) struct CargoOptions {
    #[clap(allow_hyphen_values = true)]
    remaining: Vec<String>,
}

/// Returns the build profile to use. Order of priority is (1) command line (2) cackle.toml (3)
/// default.
pub(crate) fn profile_name<'a>(args: &'a Args, config: &'a CommonConfig) -> &'a str {
    args.profile
        .as_deref()
        .or(config.profile.as_deref())
        .unwrap_or(DEFAULT_PROFILE_NAME)
}

pub(crate) fn command(
    base_command: &str,
    dir: &Path,
    args: &Args,
    config: &CommonConfig,
) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(dir);
    if args.colour.should_use_colour() {
        command.arg("--color=always");
    }
    let extra_args = match &args.command {
        Some(crate::Command::Test(cargo_options)) => {
            command.arg("test");
            cargo_options.remaining.as_slice()
        }
        Some(crate::Command::Run(cargo_options)) => {
            command.arg("run");
            cargo_options.remaining.as_slice()
        }
        _ => {
            command.arg(base_command);
            &[]
        }
    };
    command
        .arg("--config")
        .arg(format!("profile.{DEFAULT_PROFILE_NAME}.inherits=\"dev\""));
    // Optimisation would likely make it harder to figure out where code came from.
    command
        .arg("--config")
        .arg(format!("profile.{DEFAULT_PROFILE_NAME}.opt-level=0"));
    // We currently always clean before we build, so incremental compilation would just be a waste.
    command
        .arg("--config")
        .arg(format!("profile.{DEFAULT_PROFILE_NAME}.incremental=false"));
    // We don't currently support split debug info.
    command.arg("--config").arg("split-debuginfo=\"off\"");
    let profile = profile_name(args, config);
    command.arg("--profile").arg(profile);
    command.env(PROFILE_NAME_ENV, profile);
    command.args(extra_args);
    command
}
