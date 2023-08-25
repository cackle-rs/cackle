//! This crate's test checks various properties of the sandbox that it's running in.

use std::path::Path;

pub fn access_files() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir.join("crab9-test-output.txt");
    let contents = format!("{} {} {}", "DO", "NOT", "SUBMIT");

    // This write should fail because the sandbox should prevent writes to the source directory.
    if std::fs::write(&output_path, contents).is_ok() {
        panic!(
            "crab9 test was run without a sandbox. Wrote {}",
            output_path.display()
        );
    }

    // This write should succeed because the sandbox configuration for this package allows writing
    // to the scratch directory.
    let output_path = manifest_dir.join("scratch/writable.txt");
    if let Err(error) = std::fs::write(&output_path, "This file is written by a test") {
        panic!("Failed to write {}: {error}", output_path.display());
    }

    // Verify that we can't access the socket used to communicate with main cackle process.
    // CACKLE_SOCKET_PATH should always be set when we build via cackle, so we fail the test if it
    // wasn't set. That means this test can only be run via cackle. We do however want the test to
    // be able to build outside of cackle, so we use option_env! rather than env! to make it a
    // runtime error not a build-time error.
    let socket_path = option_env!("CACKLE_SOCKET_PATH")
        .map(Path::new)
        .expect("CACKLE_SOCKET_PATH not set at build time");
    if socket_path.exists() {
        panic!(
            "socket_path: `{}` accessible from test sandbox",
            socket_path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        access_files();
    }
}
