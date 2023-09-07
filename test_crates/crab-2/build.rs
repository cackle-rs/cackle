use std::path::PathBuf;
use std::process::Command;

fn main() {
    let base_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let object_file = out_dir.join("nothing.o");
    run(Command::new("cc")
        .arg("-c")
        .arg(base_dir.join("nothing.c"))
        .arg("-o")
        .arg(&object_file));
    run(Command::new("ar")
        .arg("r")
        .arg(out_dir.join("libnothing.a"))
        .arg(&object_file));
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rerun-if-changed=nothing.c");
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

// This crate already uses unsafe in regular code above. Here we define a macro that uses unsafe.
// This isn't checked by any tests, but is useful for manual testing that we're merging both sources
// of unsafe.
#[macro_export]
macro_rules! check_something {
    () => {
        let v = [42, 43];
        assert_eq!(*unsafe { v.get_unchecked(0) }, 42);
    };
}

// This unsafe usage won't be picked up by the token checker, but will be picked up by the compiler.
// This is for manual testing of how this displays in the UI.
#[no_mangle]
pub fn this_is_unsafe_too() {}
