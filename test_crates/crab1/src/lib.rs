use std::collections::HashMap;

mod impl1;

pub use crate::impl1::crab1;

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
