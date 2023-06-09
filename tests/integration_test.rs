use anyhow::Context;
use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn integration_test() -> Result<()> {
    let crate_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let cackle_exe = target_dir().join("cackle");
    let status = Command::new(&cackle_exe)
        .arg("--fail-on-warnings")
        .arg("--path")
        .arg(crate_root.join("test_crates"))
        .arg("check")
        .status()
        .with_context(|| format!("Failed to invoke `{}`", cackle_exe.display()))?;
    assert!(status.success());
    Ok(())
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
