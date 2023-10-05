use crate::config::permissions::PermSel;
use crate::config::RustcConfig;
use crate::config::SandboxConfig;
use crate::config::SandboxKind;
use crate::crate_index::CrateSel;
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
    /// Runs `command` inside the sandbox.
    fn run(&self, command: &Command) -> Result<std::process::Output>;

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
            if var.to_str().is_some_and(is_cargo_env) {
                self.set_env(OsStr::new(&var), OsStr::new(&value));
            }
        }
    }

    /// Returns an object that when displayed serves to tell the user what the sandbox would do.
    /// e.g. the command that would be run with all flags.
    fn display_to_run(&self, command: &Command) -> Box<dyn Display>;
}

pub(crate) fn from_config(config: &SandboxConfig) -> Result<Option<Box<dyn Sandbox>>> {
    let mut sandbox = match &config.kind {
        None | Some(SandboxKind::Disabled) => return Ok(None),
        Some(SandboxKind::Bubblewrap) => Box::<bubblewrap::Bubblewrap>::default(),
    };

    let home = PathBuf::from(std::env::var("HOME").context("Couldn't get HOME env var")?);
    // We allow access to the root of the filesystem, but only selected parts of the user's home
    // directory. The home directory is where sensitive stuff is most likely to live. e.g. access
    // tokens, credentials, ssh keys etc.
    sandbox.ro_bind(Path::new("/"));
    sandbox.tmpfs(&home);
    sandbox.tmpfs(Path::new("/var"));
    sandbox.tmpfs(Path::new("/tmp"));
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
    for env in &config.pass_env {
        sandbox.pass_env(env);
    }

    // Allow read access to the crate's root source directory.
    sandbox.ro_bind(Path::new(&get_env("CARGO_MANIFEST_DIR")?));

    // LD_LIBRARY_PATH is set when running `cargo test` on crates that normally compile as
    // cdylibs - e.g. proc macros. If we don't pass it through, those tests will fail to find
    // runtime dependencies.
    sandbox.pass_env("LD_LIBRARY_PATH");
    sandbox.pass_cargo_env();

    for dir in &config.bind_writable {
        if !dir.exists() {
            bail!(
                "Sandbox config says to bind directory `{}`, but that doesn't exist",
                dir.display()
            );
        }
        if !dir.is_dir() {
            bail!(
                "Sandbox config says to bind directory `{}`, but that isn't a directory",
                dir.display()
            );
        }
        sandbox.writable_bind(dir);
    }
    for dir in &config.make_writable {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory `{}`", dir.display()))?;
        sandbox.writable_bind(dir);
    }
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

/// Information extracted from the rustc command line that's relevant to running it in a sandbox.
#[derive(Default)]
pub(crate) struct RustcSandboxInputs {
    output_directories: Vec<PathBuf>,
    input_directories: Vec<PathBuf>,
    /// The names of environment variables that were set by the build script by printing
    /// "cargo:rustc-env=...". These variables should be allowed through when running rustc.
    build_script_env_vars: Vec<String>,
}

impl RustcSandboxInputs {
    pub(crate) fn from_env(crate_sel: &CrateSel) -> Result<Self> {
        let mut result = Self::default();
        let mut next_is_out = false;
        let target_dir = PathBuf::from(get_env(crate::proxy::TARGET_DIR)?);
        let manifest_dir = PathBuf::from(get_env(crate::proxy::MANIFEST_DIR)?);
        let tmpdir = PathBuf::from(get_env(crate::proxy::TMPDIR_ENV)?);
        result.input_directories.push(manifest_dir);
        for arg in std::env::args() {
            if next_is_out {
                result.output_directories.push(arg.into());
                next_is_out = false;
                continue;
            }
            next_is_out = arg == "--out-dir";
            if let Some(rest) = arg.strip_prefix("incremental=") {
                result.output_directories.push(rest.into());
            }
        }
        if let Ok(socket_path) = std::env::var(crate::proxy::SOCKET_ENV) {
            if let Some(dir) = Path::new(&socket_path).parent() {
                result.output_directories.push(dir.to_owned());
            }
        }
        result
            .output_directories
            .retain(|d| !d.starts_with(&target_dir));
        result.output_directories.push(target_dir);
        result.build_script_env_vars = read_env_vars(&tmpdir, crate_sel);
        Ok(result)
    }
}

pub(crate) fn for_rustc(
    config: &RustcConfig,
    inputs: &RustcSandboxInputs,
) -> Result<Option<Box<dyn Sandbox>>> {
    let Some(mut sandbox) = from_config(&config.sandbox)? else {
        return Ok(None);
    };
    for dir in &inputs.input_directories {
        sandbox.ro_bind(dir);
    }
    for dir in &inputs.output_directories {
        sandbox.writable_bind(dir);
    }
    for env in crate::proxy::RUSTC_ENV_VARS {
        sandbox.pass_env(env);
    }
    for env in &inputs.build_script_env_vars {
        sandbox.pass_env(env);
    }
    Ok(Some(sandbox))
}

pub(crate) fn for_perm_sel(
    config: &SandboxConfig,
    bin_path: &Path,
    perm_sel: &PermSel,
) -> Result<Option<Box<dyn Sandbox>>> {
    let Some(mut sandbox) = from_config(config)
        .with_context(|| format!("Failed to build sandbox config for `{perm_sel}`"))?
    else {
        return Ok(None);
    };

    // Allow read access to the build directory. This contains the bin file being executed and
    // possibly other binaries.
    if let Some(build_dir) = build_directory(bin_path) {
        sandbox.ro_bind(build_dir);
    }
    // Allow write access to OUT_DIR.
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        sandbox.writable_bind(Path::new(&out_dir));
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
    if kind == SandboxKind::Bubblewrap
        && std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .is_err()
    {
        anyhow::bail!("Failed to run `bwrap`, perhaps it needs to be installed? On systems with apt you can `sudo apt install bubblewrap`");
    }
    Ok(())
}

fn env_vars_file(tmpdir: &Path, crate_sel: &CrateSel) -> PathBuf {
    tmpdir.join(format!("{}.env", crate_sel.pkg_id))
}

pub(crate) fn write_env_vars(
    tmpdir: &Path,
    crate_sel: &CrateSel,
    env_vars: &[String],
) -> Result<()> {
    crate::fs::write(env_vars_file(tmpdir, crate_sel), env_vars.join("\n"))
}

fn read_env_vars(tmpdir: &Path, crate_sel: &CrateSel) -> Vec<String> {
    let filename = env_vars_file(tmpdir, crate_sel);
    // Env vars will only be written when there was a build script run, so we just ignore errors.
    let Ok(contents) = std::fs::read_to_string(filename) else {
        return Default::default();
    };
    contents
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| line.to_owned())
        .collect()
}

fn get_env(var_name: &str) -> Result<String> {
    std::env::var(var_name)
        .with_context(|| format!("Failed to get environment variable `{var_name}`"))
}

fn build_directory(executable: &Path) -> Option<&Path> {
    let parent = executable.parent()?;
    if parent.file_name().is_some_and(|n| n == "deps") {
        if let Some(grandparent) = parent.parent() {
            return Some(grandparent);
        }
    }
    Some(parent)
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
