//! This crate makes no use of any classified APIs.

use std::ops::Deref;

pub fn add(left: u32, right: u32) -> u32 {
    left + right
}

pub fn print_default<T: Default + std::fmt::Debug + Deref>() {
    println!("default: {:?}", T::default());
    // Make use of an associated type that isn't part of our function signature's generics.
    let _ = T::default().deref();
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
