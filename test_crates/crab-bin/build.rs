fn main() {
    std::fs::write(
        "scratch/written-by-build-script.txt",
        "Hello from crab-bin.build",
    )
    .unwrap();
}
