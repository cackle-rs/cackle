use std::path::Path;
use std::process::Command;

pub(crate) fn command(base_command: &str, dir: &Path) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(dir);
    command.arg(base_command);
    command.args(["--config", "profile.cackle.inherits=\"dev\""]);
    command.args(["--config", "profile.cackle.debug=1"]);
    command.args(["--config", "profile.cackle.opt-level=0"]);
    command.args(["--profile", "cackle"]);
    command
}
