extern "C" {
    fn cpp_entry_point() -> i32;
}

pub fn call_cpp_code() -> i32 {
    unsafe { cpp_entry_point() }
}

#[cfg(test)]
mod tests {
    use crate::call_cpp_code;

    #[test]
    fn test_call_cpp_code() {
        assert_eq!(call_cpp_code(), 42);
    }
}
