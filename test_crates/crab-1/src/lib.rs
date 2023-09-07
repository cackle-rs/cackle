use std::collections::HashMap;

mod impl1;

pub use crate::impl1::crab_1;

/// This struct causes hashbrown to be linked in, which, if we're not careful we can have trouble
/// identifying the source location for.
#[derive(Debug)]
pub struct HashMapWrapper {
    pub foo: HashMap<String, bool>,
}

/// This function is declared as performing filesystem access by cackle.toml. We also call it
/// ourselves, but we don't want functions that we define and call to count as permissions that
/// we're using.
pub fn read_file(_path: &str) -> Option<String> {
    None
}

pub fn call_read_file() {
    read_file("tmp.txt");
}

/// Binds a TCP port. This function is dead code, so this should not be considered.
pub fn do_network_stuff() {
    std::net::TcpListener::bind("127.0.0.1:9876").unwrap();
}

/// This function shows up in the dynamic symbols of shared1, so should count as used.
#[no_mangle]
pub extern "C" fn crab_1_entry() {
    println!("{:?}", std::env::var("HOME"));
}

/// Makes sure that we attribute this call to abort to this crate, not the crate that calls this
/// function, even though it's marked as inline(always).
#[inline(always)]
pub fn inlined_abort() {
    std::process::abort();
}

/// This function is only called from a test in crab_3.
pub fn do_unix_socket_stuff() {
    let _ = std::os::unix::net::UnixStream::pair();
}

/// A function that we restrict access to, is inlined and which calls no other functions. This tests
/// that inlined function usages are attributed correctly.
#[inline(always)]
pub fn restrict1() -> u64 {
    let mut x: u64;
    unsafe {
        std::arch::asm!(
            "mov {res}, 42",
            res = out(reg) x);
    }
    x
}
