//! This crate is significant in that two separate build.rs files depend on it. This means that any
//! problems encountered while checking this crate might show up twice, since the two build.rs files
//! can be checked in parallel.

use std::path::Path;
use std::sync::atomic::AtomicU8;

pub fn do_something() -> bool {
    helper()
}

fn helper() -> bool {
    let c = || Path::new("/").exists();
    c()
}

pub struct Metadata;

pub struct MacroCallsite {
    _interest: AtomicU8,
    meta: &'static Metadata,
}

impl MacroCallsite {
    pub const fn new(meta: &'static Metadata) -> Self {
        Self {
            _interest: AtomicU8::new(0),
            meta,
        }
    }

    #[inline(always)]
    pub fn metadata(&self) -> &Metadata {
        self.meta
    }
}
