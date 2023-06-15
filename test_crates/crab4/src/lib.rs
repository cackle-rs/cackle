//! This crate helps test having multiple uses of the same API. It also tests that we can get
//! results from this crate in parallel to crab1, since neither depends on the other.

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
