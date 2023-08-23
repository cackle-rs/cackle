use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::from_bytes(&[0xff])) {
        println!("42");
    } else {
        println!("Didn't get expected arguments. Got: {:?}", std::env::args());
    }
}
