use super::Sandbox;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt::Display;
use std::path::Path;
use std::process::Command;

#[derive(Default)]
pub(super) struct Bubblewrap {
    args: Vec<OsString>,
}

impl Bubblewrap {
    fn arg<S: AsRef<OsStr>>(&mut self, arg: S) {
        self.args.push(arg.as_ref().to_owned());
    }

    fn command(&self, command: &Command) -> Command {
        let mut bwrap_command = Command::new("bwrap");
        bwrap_command
            .args(["--unshare-all"])
            .args(["--uid", "1000"])
            .args(["--gid", "1000"])
            .args(["--hostname", "none"])
            .args(["--new-session"])
            .args(["--clearenv"])
            .args(&self.args)
            .args(["--dev", "/dev"])
            .args(["--proc", "/proc"]);
        for (var_name, value) in command.get_envs() {
            if let Some(value) = value {
                bwrap_command.arg("--setenv").arg(var_name).arg(value);
            } else {
                bwrap_command.arg("--unsetenv").arg(var_name);
            }
        }
        bwrap_command
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        bwrap_command
    }
}

impl Sandbox for Bubblewrap {
    fn raw_arg(&mut self, arg: &OsStr) {
        self.args.push(arg.to_owned());
    }

    fn tmpfs(&mut self, dir: &Path) {
        self.arg("--tmpfs");
        self.arg(dir);
    }

    fn ro_bind(&mut self, dir: &Path) {
        if !dir.exists() {
            return;
        }
        self.arg("--ro-bind");
        self.arg(dir);
        self.arg(dir);
    }

    fn writable_bind(&mut self, dir: &Path) {
        self.arg("--bind-try");
        self.arg(dir);
        self.arg(dir);
    }

    fn set_env(&mut self, var: &OsStr, value: &OsStr) {
        self.arg("--setenv");
        self.arg(var);
        self.arg(value);
    }

    fn allow_network(&mut self) {
        self.arg("--share-net");
    }

    fn run(&self, command: &Command) -> Result<std::process::Output> {
        let mut command = self.command(command);
        command.output().with_context(|| {
            format!(
                "Failed to run sandbox command: {}",
                Path::new(command.get_program()).display()
            )
        })
    }

    fn display_to_run(&self, command: &Command) -> Box<dyn Display> {
        Box::new(CommandDisplay {
            command: self.command(command),
        })
    }
}

pub(crate) fn has_bwrap() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

struct CommandDisplay {
    command: Command,
}

impl Display for CommandDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.command.get_program().to_string_lossy())?;
        for arg in self.command.get_args() {
            let arg = arg.to_string_lossy();
            if arg.contains(' ') || arg.contains('"') || arg.is_empty() {
                // Use debug print, since that gives us quotes.
                write!(f, " {:?}", arg)?;
            } else {
                // Print without quotes, since it probably isn't necessary.
                write!(f, " {arg}")?
            }
        }
        Ok(())
    }
}
