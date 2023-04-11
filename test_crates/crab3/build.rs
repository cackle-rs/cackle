use std::path::PathBuf;

fn main() {
    write_output_files();
}

fn write_output_files() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("extra_code.rs"), r#"std::env::var("PATH")"#).unwrap();
}
