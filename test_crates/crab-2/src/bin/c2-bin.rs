fn main() {
    if std::env::var("CRASH_IF_DEFINED").is_ok() {
        panic!();
    }
}
