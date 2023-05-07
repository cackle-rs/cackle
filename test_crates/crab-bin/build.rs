fn main() {
    std::fs::write("written-by-build-script.txt", "Hello from crab-bin.build").unwrap();
}
