use std::path::PathBuf;

#[test]
fn run_crab9_bin() {
    let program = target_dir().join("crab9-bin");
    match std::process::Command::new(&program).output() {
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
