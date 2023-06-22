fn main() {
    assert!(crab5::do_something());

    // Check a selection of the environment variables that cargo sets and which we should pass
    // through to build scripts.
    let variables = ["OPT_LEVEL", "PROFILE", "OUT_DIR", "CARGO", "TARGET", "HOST"];
    for var in variables {
        if let Err(std::env::VarError::NotPresent) = std::env::var(var) {
            panic!("Environment variable `{var}` not set in build script")
        }
    }
}
