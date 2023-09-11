fn main() {
    std::fs::write(
        "scratch/written-by-build-script.txt",
        "Hello from crab-bin.build",
    )
    .unwrap();

    // Tell cargo that it doesn't need to rerun this build script on every build.
    println!("cargo:rerun-if-changed=build.rs");
}
