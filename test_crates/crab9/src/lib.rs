use std::path::Path;

pub fn access_file() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir.join("crab9-test-output.txt");
    if std::fs::write(&output_path, "").is_ok() {
        panic!(
            "crab9 test was run without a sandbox. Wrote {}",
            output_path.display()
        );
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
