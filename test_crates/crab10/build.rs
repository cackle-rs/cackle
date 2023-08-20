use std::path::PathBuf;
use std::process::Command;

fn main() {
    let base_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let object_file = out_dir.join("cpp_stuff.o");
    run(Command::new("c++")
        .arg("-g")
        .arg("-c")
        .arg(base_dir.join("cpp_stuff.cc"))
        .arg("-o")
        .arg(&object_file));
    run(Command::new("ar")
        .arg("r")
        .arg(out_dir.join("libcpp_stuff.a"))
        .arg(&object_file));
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rustc-link-lib=cpp_stuff",);
    println!("cargo:rerun-if-changed=cpp_stuff.cc");
    let v = [42, 43];
    assert_eq!(*unsafe { v.get_unchecked(0) }, 42);
    assert_eq!(*unsafe { v.get_unchecked(1) }, 43);
}

fn run(cmd: &mut Command) {
    match cmd.status() {
        Ok(status) => {
            if status.code() != Some(0) {
                panic!("Command exited with non-zero status while running:\n{cmd:?}");
            }
        }
        Err(_) => {
            panic!("Failed to run {cmd:?}");
        }
    }
}
