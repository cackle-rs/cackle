use anyhow::Context;
use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn integration_test() -> Result<()> {
    fn run_with_args(tmpdir: &TempDir, args: &[&str], expect_failure: bool) -> Result<String> {
        let mut command = Command::new(cackle_exe());
        // Remove cargo and rust-related environment variables. In particular we want to remove
        // variables that cargo sets, but which won't always be set. For example CARGO_PKG_NAME is
        // set by cargo when it invokes rustc, but only when it's compiling a package, not when it
        // queries rustc for version information. If we allow such variables to pass through, then
        // our code that proxies rustc can appear to work from the test, but only because the test
        // itself was run from cargo.
        for (var, _) in std::env::vars() {
            if var.starts_with("CARGO") || var.starts_with("RUST") {
                command.env_remove(var);
            }
        }
        // Delete everything from our tmpdir. Deleting files makes sure that we don't mask bugs that
        // would only occur with a fresh temporary directory. We don't delete the directory itself
        // because recreating it with the same name could be a security issue on a shared system.
        for entry in tmpdir.path().read_dir()? {
            let entry = entry?;
            if entry.path().is_dir() {
                std::fs::remove_dir_all(entry.path())
            } else {
                std::fs::remove_file(entry.path())
            }
            .with_context(|| format!("Failed to remove `{}`", entry.path().display()))?;
        }
        let root = crate_root().join("test_crates");
        let output = command
            .env("CARGO_TARGET_DIR", "custom_target_dir")
            .arg("acl")
            .arg("--fail-on-warnings")
            .arg("--save-requests")
            .arg("--path")
            .arg(&root)
            // Use the same tmpdir for all our runs. This speeds up this test because many of our
            // tests depend on CACKLE_SOCKET_PATH, so would otherwise need to be rebuilt whenever it
            // changes.
            .arg("--tmpdir")
            .arg(tmpdir.path())
            .arg("--ui=none")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .output()
            .with_context(|| format!("Failed to invoke `{}`", cackle_exe().display()))?;

        let stdout = std::str::from_utf8(&output.stdout).unwrap().to_owned();
        let stderr = std::str::from_utf8(&output.stderr).unwrap().to_owned();
        if expect_failure {
            if output.status.success() {
                panic!("Test succeeded when we expected it to fail. Output:\n{stdout}\n{stderr}");
            }
        } else if !output.status.success() {
            panic!("Test failed when we expected it to succeed. Output:\n{stdout}\n{stderr}");
        }
        Ok(stdout)
    }

    let tmpdir = TempDir::new()?;

    run_with_args(&tmpdir, &[], false)?;

    // Trigger crab-2 to rebuild its test, but not rerun its build script. This ensures that
    // variables set by the build script survive between runs even if the build script doesn't
    // rerun.
    std::env::set_var("CRAB_2_EXT_ENV", "1");

    run_with_args(&tmpdir, &["test", "-v"], false)?;
    let out = run_with_args(
        &tmpdir,
        &["run", "--bin", "c2-bin", "--", "40", "4", "-2"],
        false,
    )?;
    let out = out.trim();
    let n: i32 = match out.parse() {
        Ok(x) => x,
        Err(_) => panic!("Unexpected output. Expected integer, got `{out}`"),
    };
    assert_eq!(n, 42);

    std::env::set_var("CRAB_9_CRASH_TEST", "1");
    let out = run_with_args(
        &tmpdir,
        &[
            "--features",
            "",
            "test",
            "-p",
            "crab-9",
            "conditional_crash",
        ],
        true,
    )?;
    if !out.contains("Deliberate crash") {
        panic!("Test failed, but didn't contain expected message. Output was:\n{out}");
    }

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
        .arg("acl")
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
    target_dir().join("cargo-acl")
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
