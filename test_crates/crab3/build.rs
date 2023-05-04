use std::path::Path;
use std::path::PathBuf;

fn main() {
    write_output_files();

    // Generally RUSTC_WRAPPER shouldn't be set when we run our build script, but if it is, the path
    // it points to should exist.
    if let Ok(rustc_wrapper) = std::env::var("RUSTC_WRAPPER") {
        if !Path::new(&rustc_wrapper).exists() {
            panic!("RUSTC_WRAPPER is set to {rustc_wrapper}, but that doesn't exist");
        }
    }
}

fn write_output_files() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("extra_code.rs"), r#"std::env::var("PATH")"#).unwrap();
    let home = PathBuf::from(std::env::var("HOME").unwrap());

    if !cfg!(feature = "crash-if-not-sandboxed") {
        return;
    }

    // This file shouldn't exist in the sandbox, even if it exists outside it.
    let credentials_path = home.join(".cargo/credentials");
    if std::fs::read(&credentials_path).is_ok() {
        panic!(
            "We shouldn't be able to read {}",
            credentials_path.display()
        );
    }

    // We shouldn't be able to write to the cargo registry.
    let registry = home.join(".cargo/registry");
    if !registry.exists() {
        panic!("{} should exist", registry.display());
    }
    let file_to_write = registry.join("cannot-write-here.txt");
    if std::fs::write(&file_to_write, "test").is_ok() {
        std::fs::remove_file(&file_to_write).unwrap();
        panic!("We shouldn't be able to write {}", file_to_write.display());
    }
}
