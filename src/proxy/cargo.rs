use crate::Args;
use std::path::Path;
use std::process::Command;

/// The name of the default cargo profile that we use.
pub(crate) const DEFAULT_PROFILE_NAME: &str = "cackle";

pub(crate) fn command(base_command: &str, dir: &Path, args: &Args) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(dir);
    if args.colour.should_use_colour() {
        command.arg("--color=always");
    }
    command.arg(base_command);
    command
        .arg("--config")
        .arg(format!("profile.{DEFAULT_PROFILE_NAME}.inherits=\"dev\""));
    // We need debug information so that we know where code came from and can attribute symbol
    // references to a particular crate. Level 1 is sufficient for code within functions, but we
    // need level 2 in order to have debug information for variables.
    let debug_level = 2;
    command.arg("--config").arg(format!(
        "profile.{DEFAULT_PROFILE_NAME}.debug={debug_level}"
    ));
    command.arg("--config").arg(format!(
        "profile.{DEFAULT_PROFILE_NAME}.build-override.debug={debug_level}"
    ));
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
    command.arg("--profile").arg(&args.profile);
    command
}
