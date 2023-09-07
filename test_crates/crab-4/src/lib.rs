//! This crate helps test having multiple uses of the same API. It also tests that we can get
//! results from this crate in parallel to crab_1, since neither depends on the other.

use std::ffi::OsString;
use std::path::Path;

pub fn access_file() {
    f1();
    f2();
    f3();
    f4();
    let mut s = std::mem::ManuallyDrop::new("Hello".to_owned());
    let s2 = unsafe { String::from_raw_parts(s.as_mut_ptr(), s.len(), s.capacity()) };
    println!("{s2}");
}

fn f1() {
    let _ = Path::new("a.txt").exists();
}

fn f2() {
    let _ = Path::new("a.txt").exists();
}

fn f3() {
    let _ = Path::new("a.txt").exists();
}

fn f4() {
    let _ = Path::new("a.txt").exists();
}

/// Make sure that we can't circumvent checks by accessing a function via a function pointer instead
/// of a direct function call.
static GET_ENV: &[&(dyn (Fn(&'static str) -> Option<OsString>) + Sync + 'static)] =
    &[&std::env::var_os::<&'static str>];

pub fn get_home() -> Option<OsString> {
    (GET_ENV[0])("HOME")
}

/// Same thing as `GET_ENV`, but make sure it works across crate boundaries.
pub static GET_PID: &[&(dyn (Fn() -> u32) + Sync + 'static)] = &[&std::process::id];
