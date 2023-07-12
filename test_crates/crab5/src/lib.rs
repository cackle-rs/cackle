//! This crate is significant in that two separate build.rs files depend on it. This means that any
//! problems encountered while checking this crate might show up twice, since the two build.rs files
//! can be checked in parallel.

use std::path::Path;

pub fn do_something() -> bool {
    helper()
}

fn helper() -> bool {
    let c = || Path::new("/").exists();
    c()
}
