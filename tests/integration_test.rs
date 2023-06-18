use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn integration_test() -> Result<()> {
    let status = Command::new(cackle_exe())
        .arg("--fail-on-warnings")
        .arg("--path")
        .arg(crate_root().join("test_crates"))
        .arg("check")
        .status()
        .with_context(|| format!("Failed to invoke `{}`", cackle_exe().display()))?;
    assert!(status.success());
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
        .arg("check")
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
