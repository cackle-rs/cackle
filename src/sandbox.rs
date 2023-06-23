use crate::config::SandboxConfig;
use crate::config::SandboxKind;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use std::ffi::OsStr;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

mod bubblewrap;

pub(crate) trait Sandbox {
    /// Runs `binary` inside the sandbox.
    fn run(&self, binary: &Path) -> Result<std::process::Output>;

    /// Bind a tmpfs at `dir`.
    fn tmpfs(&mut self, dir: &Path);

    /// Set the environment variable `var` to `value`.
    fn set_env(&mut self, var: &OsStr, value: &OsStr);

    /// Bind `dir` into the sandbox read-only.
    fn ro_bind(&mut self, dir: &Path);

    /// Bind `dir` into the sandbox writable.
    fn writable_bind(&mut self, dir: &Path);

    /// Allow unrestricted network access.
    fn allow_network(&mut self);

    /// Append a sandbox-specific argument.
    fn raw_arg(&mut self, arg: &OsStr);

    /// Pass through the value of `env_var_name`
    fn pass_env(&mut self, env_var_name: &str) {
        if let Ok(value) = std::env::var(env_var_name) {
            self.set_env(OsStr::new(env_var_name), OsStr::new(&value));
        }
    }

    /// Pass through all cargo environment variables.
    fn pass_cargo_env(&mut self) {
        self.pass_env("OUT_DIR");
        for (var, value) in std::env::vars_os() {
            if var.to_str().map(is_cargo_env).unwrap_or(false) {
                self.set_env(OsStr::new(&var), OsStr::new(&value));
            }
        }
    }

    /// Returns an object that when displayed serves to tell the user what the sandbox would do.
    /// e.g. the command that would be run with all flags.
    fn display_to_run(&self, binary: &Path) -> Box<dyn Display>;
}

pub(crate) fn from_config(config: &SandboxConfig) -> Result<Option<Box<dyn Sandbox>>> {
    let mut sandbox = match &config.kind {
        SandboxKind::Disabled | SandboxKind::Inherit => return Ok(None),
        SandboxKind::Bubblewrap => Box::<bubblewrap::Bubblewrap>::default(),
    };
    for dir in &config.allow_read {
        sandbox.ro_bind(Path::new(dir));
    }
    let home = PathBuf::from(std::env::var("HOME").context("Couldn't get HOME env var")?);
    // We allow access to the root of the filesystem, but only selected parts of the user's home
    // directory. The home directory is where sensitive stuff is most likely to live. e.g. access
    // tokens, credentials, ssh keys etc.
    sandbox.ro_bind(Path::new("/"));
    sandbox.tmpfs(&home);
    sandbox.tmpfs(Path::new("/var"));
    sandbox.tmpfs(Path::new("/tmp"));
    sandbox.tmpfs(Path::new("/usr/share"));
    // We need access to some parts of ~/.cargo in order to be able to build, but we don't bind all
    // of it because it might contain crates.io credentials, which we'd like to avoid exposing.
    let cargo_home = &home.join(".cargo");
    sandbox.ro_bind(&cargo_home.join("bin"));
    sandbox.ro_bind(&cargo_home.join("git"));
    sandbox.ro_bind(&cargo_home.join("registry"));
    sandbox.ro_bind(&home.join(".rustup"));
    sandbox.set_env(OsStr::new("USER"), OsStr::new("user"));
    sandbox.pass_env("PATH");
    sandbox.pass_env("HOME");
    for arg in &config.extra_args {
        sandbox.raw_arg(OsStr::new(arg));
    }
    if config.allow_network.unwrap_or(false) {
        sandbox.allow_network();
    } else {
        // Only allow access to the real /run when network access is permitted, otherwise mount a
        // tmpfs there to prevent access to the real contents. Doing this when network access is
        // permitted prevents DNS lookups on some systems.
        sandbox.tmpfs(Path::new("/run"));
    }
    Ok(Some(sandbox))
}

pub(crate) fn available_kind() -> SandboxKind {
    if bubblewrap::has_bwrap() {
        SandboxKind::Bubblewrap
    } else {
        SandboxKind::Disabled
    }
}

pub(crate) fn verify_kind(kind: SandboxKind) -> Result<()> {
    if kind == SandboxKind::Bubblewrap && Command::new("bwrap").arg("--version").output().is_err() {
        bail!("Failed to run `bwrap`, perhaps it needs to be installed? On systems with apt you can `sudo apt install bubblewrap`");
    }
    Ok(())
}

fn is_cargo_env(var: &str) -> bool {
    // We set this when we call cargo. We don't want it passed through to build scripts.
    if var == "RUSTC_WRAPPER" {
        return false;
    }

    const PREFIXES: &[&str] = &["CARGO", "RUSTC", "DEP_"];
    const ONE_OFFS: &[&str] = &[
        "TARGET",
        "OPT_LEVEL",
        "PROFILE",
        "HOST",
        "NUM_JOBS",
        "DEBUG",
    ];
    PREFIXES.iter().any(|prefix| var.starts_with(prefix)) || ONE_OFFS.contains(&var)
}
