use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("extra_code.rs"), r#"std::env::var("PATH")"#).unwrap();
}
