pub fn print_defaults() {
    // Instantiate cra6::print_default with a PathBuf. This shouldn't count as crab_6 using the fs
    // API, but it should count as this crate using it.
    crab_6::print_default::<std::path::PathBuf>();
}
