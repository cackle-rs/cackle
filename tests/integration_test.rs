use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn integration_test() -> Result<()> {
    fn run_with_args(tmpdir: &TempDir, args: &[&str]) -> Result<()> {
        let mut command = Command::new(cackle_exe());
        // Remove cargo and rust-releated environment variables. In particular we want to remove
        // variables that cargo sets, but which won't always be set. For example CARGO_PKG_NAME is set
        // by cargo when it invokes rustc, but only when it's compiling a package, not when it queries
        // rustc for version information. If we allow such variables to pass through, then our code that
        // proxies rustc can appear to work from the test, but only because the test itself was run from
        // cargo.
        for (var, _) in std::env::vars() {
            if var.starts_with("CARGO") || var.starts_with("RUST") {
                command.env_remove(var);
            }
        }
        let status = command
            .arg("--fail-on-warnings")
            .arg("--save-requests")
            .arg("--path")
            .arg(crate_root().join("test_crates"))
            // Use the same tmpdir for all our runs. This speeds up this test because many of our
            // tests depend on CACKLE_SOCKET_PATH, so would otherwise need to be rebuilt whenever it
            // changes.
            .arg("--tmpdir")
            .arg(tmpdir.path())
            .arg("--ui=none")
            .args(args)
            .status()
            .with_context(|| format!("Failed to invoke `{}`", cackle_exe().display()))?;
        assert!(status.success());
        Ok(())
    }

    let tmpdir = TempDir::new()?;

    run_with_args(&tmpdir, &[])?;
    run_with_args(&tmpdir, &["test", "-v"])?;

    Ok(())
}

/// Makes sure that if we supply an invalid toml file, that the error message includes details of
/// the problem.
#[test]
fn invalid_config() -> Result<()> {
    let tmpdir = tempfile::tempdir()?;
    let dir = tmpdir.path().join("foo");
    create_cargo_dir(&dir);
    let config_path = dir.join("cackle.toml");
    std::fs::write(config_path, "invalid_key = true")?;
    let output = Command::new(cackle_exe())
        .arg("--path")
        .arg(dir)
        .arg("--ui=none")
        .output()
        .with_context(|| format!("Failed to invoke `{}`", cackle_exe().display()))?;
    assert!(!output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    if !stdout.contains("invalid_key") {
        println!("=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}");
        panic!("Error doesn't mention invalid_key");
    }
    Ok(())
}

fn create_cargo_dir(dir: &Path) {
    Command::new("cargo")
        .arg("new")
        .arg("--vcs")
        .arg("none")
        .arg("--offline")
        .arg(dir)
        .status()
        .expect("Failed to run `cargo new`");
}

fn cackle_exe() -> PathBuf {
    target_dir().join("cackle")
}

fn crate_root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
}

fn target_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
