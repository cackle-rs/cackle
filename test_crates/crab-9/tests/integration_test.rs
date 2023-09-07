use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::PathBuf;

#[test]
fn run_crab_9_bin() {
    let program = target_dir().join("crab9-bin");
    let mut command = std::process::Command::new(&program);

    // Make sure that we can pass non-UTF-8 arguments to our binary. This can break if we let cackle
    // try to parse the arguments with clap.
    command.arg(OsStr::from_bytes(&[0xff]));

    match command.output() {
        Ok(output) => {
            let stdout = &String::from_utf8(output.stdout).unwrap();
            let stderr = &String::from_utf8(output.stderr).unwrap();
            if stdout.trim() != "42" {
                println!("=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}");
                panic!("Unexpected output");
            }
        }
        Err(error) => panic!("Failed to run {}: {}", program.display(), error),
    }
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
