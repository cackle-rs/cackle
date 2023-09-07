//! This crate makes no use of any classified APIs.

use std::ops::Deref;

pub use crab_5::MacroCallsite;
pub use crab_5::Metadata;

pub fn add(left: u32, right: u32) -> u32 {
    left + right
}

pub fn print_default<T: Default + std::fmt::Debug + Deref>() {
    println!("default: {:?}", T::default());
    // Make use of an associated type that isn't part of our function signature's generics.
    let _ = T::default().deref();
}

pub trait Foo {
    fn foo(&self);
    fn foo2(&self);
}

#[macro_export]
macro_rules! impl_foo {
    ($name:ident) => {
        pub struct $name;

        impl $crate::Foo for $name {
            fn foo(&self) {}
            fn foo2(&self) {
                self.foo();
            }
        }
    };
}

/// This macro, together with some code in crab_5, reproduces a minimal subset of a structure present
/// in the tracing/tracing-core crates. If the debug macro is invoked from another crate, say res1,
/// then we observe a reference `crab_5::MacroCallsite::metadata -> res1::print_something::CALLSITE`.
/// If `res1` is a restricted API, then crab_5 is flagged as using that restricted API, even though
/// it cannot, since crab_5 doesn't depend on res1.
#[macro_export]
macro_rules! debug {
    () => {{
        static META: $crate::Metadata = $crate::Metadata;
        static CALLSITE: $crate::MacroCallsite = $crate::MacroCallsite::new(&META);
        let meta = CALLSITE.metadata();
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
