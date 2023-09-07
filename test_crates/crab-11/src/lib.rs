//! This crate has a test that tries to write to a file in the source directory. This should succeed
//! because the sandbox is disabled for this crate.

use std::path::Path;

pub fn access_file() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir.join("scratch").join("test-output.txt");
    std::fs::write(output_path, "crab_11 test output").unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        access_file();
    }
}
