//! This crate's test tries to write to a file in the source directory. This should fail because the
//! test should be run in a sandbox where the source directory isn't writable. If writing the file
//! succeeds, then the test fails.

use std::path::Path;

pub fn access_file() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir.join("crab9-test-output.txt");
    let contents = format!("{} {} {}", "DO", "NOT", "SUBMIT");
    if std::fs::write(&output_path, contents).is_ok() {
        panic!(
            "crab9 test was run without a sandbox. Wrote {}",
            output_path.display()
        );
    }

    let output_path = manifest_dir.join("scratch/writable.txt");
    if let Err(error) = std::fs::write(&output_path, "This file is written by a test") {
        panic!("Failed to write {}: {error}", output_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        access_file();
    }
}
