#[macro_export]
macro_rules! foo {
    () => {
        std::process::exit(1)
    };
}

#[macro_export]
macro_rules! macro_that_uses_unsafe {
    ($a:expr) => {
        let v = $a;
        let mut x = 0_u32;
        if v {
            x = unsafe { core::mem::transmute(-10_i32) };
        }
        x
    };
}

pub fn do_stuff() {
    let path = include!(concat!(env!("OUT_DIR"), "/extra_code.rs"));
    println!("{path:?}");
    crab1::read_file("");
    fs::do_stuff();
    terminate::do_stuff();
    foo!();
}

#[test]
fn test_do_unix_socket_stuff() {
    crab1::do_unix_socket_stuff();
}

/// We don't actually do any filesystem-related stuff here, but we provide a module named "fs" to
/// confirm that we detect this as a possible exported API.
pub mod fs {
    pub(super) fn do_stuff() {}
}

/// As for the fs module. We need two modules, one to ignore and one to classify as an API.
pub mod terminate {
    pub(super) fn do_stuff() {}
}
