use std::path::Path;
use std::process::Command;

pub(crate) fn command(base_command: &str, dir: &Path) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(dir);
    command.arg(base_command);
    command.args(["--config", "profile.cackle.inherits=\"dev\""]);
    // We need debug information so that we know where code came from and can attribute symbol
    // references to a particular crate.
    command.args(["--config", "profile.cackle.debug=1"]);
    // Optimisation would likely make it harder to figure out where code came from.
    command.args(["--config", "profile.cackle.opt-level=0"]);
    // We currently always clean before we build, so incremental compilation would just be a waste.
    command.args(["--config", "profile.cackle.incremental=false"]);
    command.args(["--profile", "cackle"]);
    command
}
