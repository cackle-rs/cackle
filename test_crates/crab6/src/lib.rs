//! This crate makes no use of any classified APIs.

pub fn add(left: u32, right: u32) -> u32 {
    left + right
}

pub fn print_default<T: Default + std::fmt::Debug>() {
    println!("default: {:?}", T::default());
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
