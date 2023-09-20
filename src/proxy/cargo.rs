use crate::config::CommonConfig;
use crate::Args;
use clap::Parser;
use std::path::Path;
use std::process::Command;

/// The name of the default cargo profile that we use.
pub(crate) const DEFAULT_PROFILE_NAME: &str = "cackle";

#[derive(Parser, Debug, Clone)]
pub(crate) struct CargoOptions {
    subcommand: String,

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
    let extra_args;
    if let Some(crate::Command::Cargo(cargo_options)) = &args.command {
        command.arg(&cargo_options.subcommand);
        extra_args = cargo_options.remaining.as_slice();
    } else {
        command.arg(base_command);
        extra_args = &[];
    }
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
    command.arg("--profile").arg(profile_name(args, config));
    command.args(extra_args);
    command
}
