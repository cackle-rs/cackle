use super::Sandbox;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

#[derive(Default)]
pub(super) struct Bubblewrap {
    args: Vec<OsString>,
}

impl Sandbox for Bubblewrap {
    fn arg(&mut self, arg: &OsStr) {
        self.args.push(arg.to_owned());
    }

    fn tmpfs(&mut self, dir: &Path) {
        self.arg(OsStr::new("--tmpfs"));
        self.arg(dir.as_os_str());
    }

    fn ro_bind(&mut self, dir: &Path) {
        self.arg(OsStr::new("--ro-bind"));
        let dir = dir.as_ref();
        self.arg(dir);
        self.arg(dir);
    }

    fn writable_bind(&mut self, dir: &Path) {
        let dir = dir.as_ref();
        self.arg(OsStr::new("--bind-try"));
        self.arg(dir);
        self.arg(dir);
    }

    fn set_env(&mut self, var: &OsStr, value: &OsStr) {
        self.arg(OsStr::new("--setenv"));
        self.arg(var);
        self.arg(value);
    }

    fn run(&self, binary: &Path) -> Result<std::process::Output> {
        let mut command = Command::new("bwrap");
        command
            .args(&["--dev", "/dev"])
            .args(&["--proc", "/proc"])
            .args(&["--unshare-all"])
            .args(&["--uid", "1000"])
            .args(&["--gid", "1000"])
            .args(&["--hostname", "none"])
            .args(&["--new-session"])
            .args(&["--clearenv"])
            .args(&self.args)
            .arg(binary);
        command.output().with_context(|| {
            format!(
                "Failed to run sandbox command: {}",
                Path::new(command.get_program()).display()
            )
        })
    }
}
