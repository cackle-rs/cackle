use crate::colour::Colour;
use std::path::Path;
use std::process::Command;

/// The name of the cargo profile that we use.
pub(crate) const PROFILE_NAME: &str = "cackle";

pub(crate) fn command(base_command: &str, dir: &Path, colour: Colour) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(dir);
    if colour.should_use_colour() {
        command.arg("--color=always");
    }
    command.arg(base_command);
    command
        .arg("--config")
        .arg(format!("profile.{PROFILE_NAME}.inherits=\"dev\""));
    // We need debug information so that we know where code came from and can attribute symbol
    // references to a particular crate.
    command
        .arg("--config")
        .arg(format!("profile.{PROFILE_NAME}.debug=1"));
    // Optimisation would likely make it harder to figure out where code came from.
    command
        .arg("--config")
        .arg(format!("profile.{PROFILE_NAME}.opt-level=0"));
    // We currently always clean before we build, so incremental compilation would just be a waste.
    command
        .arg("--config")
        .arg(format!("profile.{PROFILE_NAME}.incremental=false"));
    command.arg("--profile").arg(PROFILE_NAME);
    command
}
