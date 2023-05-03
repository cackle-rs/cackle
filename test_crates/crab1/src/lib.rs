use std::collections::HashMap;

mod impl1;

pub use crate::impl1::crab1;

/// This struct causes hashbrown to be linked in, which, if we're not careful we can have trouble
/// identifying the source location for.
#[derive(Debug)]
pub struct HashMapWrapper {
    pub foo: HashMap<String, bool>,
}
