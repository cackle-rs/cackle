use crate::config::SandboxConfig;
use crate::config::SandboxKind;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use std::path::Path;

pub(crate) struct SandboxCommand {
    pub(crate) command_line: Vec<String>,
}

impl SandboxCommand {
    pub(crate) fn from_config(config: &SandboxConfig, crate_root: &Path) -> Result<Option<Self>> {
        let mut sandbox = Self {
            command_line: vec![],
        };
        match &config.kind {
            SandboxKind::Disabled => return Ok(None),
            SandboxKind::Bubblewrap => {
                let home = std::env::var("HOME")?;
                let crate_root = crate_root
                    .to_str()
                    .ok_or_else(|| anyhow!("Crate root needs to be valid UTF-8, but isn't"))?;
                let target_dir = format!("{crate_root}/target");
                std::fs::create_dir_all(&target_dir)
                    .with_context(|| format!("Failed to create target directory {target_dir}"))?;
                sandbox.args(&["bwrap"]);
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
                sandbox.ro_bind(crate_root);
                sandbox.writable_bind(&target_dir);
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

    pub(crate) fn args(&mut self, args: &[&str]) {
        self.command_line
            .extend(args.iter().map(|arg| -> String { arg.to_string() }))
    }

    fn tmpfs(&mut self, dir: &str) {
        self.args(&["--tmpfs", dir])
    }

    fn ro_bind(&mut self, dir: &str) {
        self.args(&["--ro-bind", dir, dir])
    }

    fn writable_bind(&mut self, dir: &str) {
        self.args(&["--bind-try", dir, dir])
    }

    fn pass_env(&mut self, env_var_name: &str) {
        if let Ok(value) = std::env::var(env_var_name) {
            self.args(&["--setenv", env_var_name, &value]);
        }
    }
}
