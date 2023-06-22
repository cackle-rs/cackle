//! This module tokenises Rust code and looks for the unsafe keyword. This is done as an additional
//! layer of defence in addition to use of the -Funsafe-code flag when compiling crates, since that
//! flag unfortunately doesn't completely prevent use of unsafe.

use crate::proxy::errors::UnsafeUsage;
use anyhow::Context;
use anyhow::Result;
use std::path::Path;

pub(crate) fn scan_path(path: &Path) -> Result<Option<UnsafeUsage>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Failed to read `{}`", path.display()))?;
    let Ok(source) = std::str::from_utf8(&bytes) else {
        // If the file isn't valid UTF-8 then we don't need to check it for the unsafe keyword,
        // since it can't be a source file that the rust compiler would accept.
        return Ok(None);
    };
    Ok(scan_string(source, path))
}

fn scan_string(source: &str, path: &Path) -> Option<UnsafeUsage> {
    let mut offset = 0;
    for token in rustc_ap_rustc_lexer::tokenize(source) {
        let new_offset = offset + token.len;
        let token_text = &source[offset..new_offset];
        if token_text == "unsafe" {
            return Some(UnsafeUsage {
                file_name: path.to_owned(),
                start_line: source[..offset].chars().filter(|ch| *ch == '\n').count() as u32 + 1,
            });
        }
        offset = new_offset;
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::unsafe_checker::scan_path;
    use crate::unsafe_checker::scan_string;
    use std::path::Path;

    fn unsafe_line(source: &str) -> Option<u32> {
        scan_string(source, Path::new("test.rs")).map(|usage| usage.start_line)
    }

    #[test]
    fn test_scan_string() {
        assert_eq!(unsafe_line("unsafe fn foo() {}"), Some(1));
        assert_eq!(unsafe_line(r#"fn foo() -> &'static str {"unsafe"}"#), None);
        assert_eq!(unsafe_line("fn foo() { unsafe {} }"), Some(1));
        assert_eq!(
            unsafe_line(
                r#"fn foo() {
                    unsafe {
                    }
                }"#
            ),
            Some(2)
        );
    }

    #[track_caller]
    fn has_unsafe_in_file(path: &str) -> bool {
        let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set");
        let root = Path::new(&root);
        scan_path(&root.join(path)).unwrap().is_some()
    }

    #[test]
    fn test_scan_test_crates() {
        assert!(!has_unsafe_in_file("test_crates/crab1/src/lib.rs"));
        assert!(has_unsafe_in_file("test_crates/crab1/src/impl1.rs"));
        assert!(!has_unsafe_in_file("test_crates/crab2/src/lib.rs"));
        assert!(has_unsafe_in_file("test_crates/crab3/src/lib.rs"));
        assert!(has_unsafe_in_file("test_crates/crab-bin/src/main.rs"));
    }
}
