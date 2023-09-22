fn main() {
    if std::env::var("CRASH_IF_DEFINED").is_ok() {
        panic!();
    }
    let total: i32 = std::env::args()
        .skip(1)
        .map(|arg| -> i32 { arg.parse().unwrap_or_default() })
        .sum();
    println!("{total}");
}
