use crate::config::SandboxConfig;
use crate::config::SandboxKind;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

pub(crate) struct SandboxCommand {
    pub(crate) command: Command,
}

impl SandboxCommand {
    pub(crate) fn from_config(config: &SandboxConfig) -> Result<Option<Self>> {
        let mut sandbox;
        match &config.kind {
            SandboxKind::Disabled => return Ok(None),
            SandboxKind::Bubblewrap => {
                let home = std::env::var("HOME").context("Couldn't get HOME env var")?;
                sandbox = SandboxCommand {
                    command: Command::new("bwrap"),
                };
                for dir in &config.allow_read {
                    sandbox.ro_bind(dir);
                }
                // TODO: Reasses if we want to list these here or just have the user list them in
                // their allow_read config.
                sandbox.ro_bind("/usr");
                sandbox.ro_bind("/lib");
                sandbox.ro_bind("/lib64");
                sandbox.ro_bind("/bin");
                sandbox.ro_bind("/etc/alternatives");
                // Note, we don't bind all of ~/.cargo because it might contain
                // crates.io credentials, which we'd like to avoid exposing.
                sandbox.ro_bind(&format!("{home}/.cargo/bin"));
                sandbox.ro_bind(&format!("{home}/.cargo/git"));
                sandbox.ro_bind(&format!("{home}/.cargo/registry"));
                sandbox.ro_bind(&format!("{home}/.rustup"));
                sandbox.tmpfs("/var");
                sandbox.tmpfs("/tmp");
                sandbox.tmpfs("/run");
                sandbox.tmpfs("/usr/share");
                sandbox.args(&["--dev", "/dev"]);
                sandbox.args(&["--proc", "/proc"]);
                sandbox.args(&["--unshare-all"]);
                sandbox.args(&["--uid", "1000"]);
                sandbox.args(&["--gid", "1000"]);
                sandbox.args(&["--hostname", "none"]);
                sandbox.args(&["--new-session"]);
                sandbox.args(&["--clearenv"]);
                sandbox.args(&["--setenv", "USER", "user"]);
                sandbox.pass_env("PATH");
                sandbox.pass_env("HOME");
                for arg in &config.extra_args {
                    sandbox.args(&[arg]);
                }
            }
        }
        Ok(Some(sandbox))
    }

    pub(crate) fn arg<S: AsRef<OsStr>>(&mut self, arg: S) {
        self.command.arg(arg);
    }

    pub(crate) fn args(&mut self, args: &[&str]) {
        self.command.args(args);
    }

    pub(crate) fn tmpfs(&mut self, dir: &str) {
        self.args(&["--tmpfs", dir])
    }

    pub(crate) fn ro_bind<S: AsRef<OsStr>>(&mut self, dir: S) {
        self.arg("--ro-bind");
        let dir = dir.as_ref();
        self.arg(dir);
        self.arg(dir);
    }

    pub(crate) fn writable_bind(&mut self, dir: &str) {
        self.args(&["--bind-try", dir, dir])
    }

    pub(crate) fn pass_env(&mut self, env_var_name: &str) {
        if let Ok(value) = std::env::var(env_var_name) {
            self.args(&["--setenv", env_var_name, &value]);
        }
    }

    /// Pass through all cargo environment variables.
    pub(crate) fn pass_cargo_env(&mut self) {
        self.pass_env("OUT_DIR");
        for (var, value) in std::env::vars_os() {
            self.arg("--setenv");
            self.arg(var);
            self.arg(value);
        }
    }

    pub(crate) fn command_to_run(mut self, binary: &Path) -> Command {
        self.arg(binary);
        self.command
    }
}
