//! Handles parsing of errors from rustc.

use crate::checker::SourceLocation;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

/// Returns source locations for all errors related to use of unsafe code in `output`, which should
/// be the output from rustc with --error-format=json.
pub(crate) fn get_disallowed_unsafe_locations(
    rustc_output: &std::process::Output,
) -> Result<Vec<SourceLocation>> {
    let stderr =
        std::str::from_utf8(&rustc_output.stderr).context("rustc emitted invalid UTF-8")?;
    Ok(get_disallowed_unsafe_locations_str(stderr))
}

fn get_disallowed_unsafe_locations_str(output: &str) -> Vec<SourceLocation> {
    let mut locations = Vec::new();
    //let workspace_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<Message>(line) else {
            continue;
        };
        if message.level == "error" && message.code.code == "unsafe_code" {
            if let Some(first_span) = message.spans.first() {
                let filename = Path::new(&first_span.file_name);
                locations.push(SourceLocation {
                    filename: std::fs::canonicalize(filename)
                        .unwrap_or_else(|_| filename.to_owned()),
                    line: first_span.line_start,
                    column: Some(first_span.column_start),
                });
            }
        }
    }
    locations
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
struct Message {
    code: Code,
    level: String,
    spans: Vec<SpannedMessage>,
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
struct Code {
    code: String,
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
struct SpannedMessage {
    file_name: String,
    line_start: u32,
    column_start: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        assert_eq!(get_disallowed_unsafe_locations_str(""), vec![]);
    }

    #[test]
    fn test_unsafe_error() {
        let json = r#"{
            "code": {"code": "unsafe_code"},
            "level": "error",
            "spans": [
                {
                    "file_name": "src/main.rs",
                    "line_start": 10,
                    "column_start": 20
                }
            ],
            "rendered": "Stuff that we don't parse"
        }"#
        .replace('\n', "");
        assert_eq!(
            get_disallowed_unsafe_locations_str(&json),
            vec![SourceLocation {
                filename: std::fs::canonicalize(Path::new("src/main.rs")).unwrap(),
                line: 10,
                column: Some(20),
            }]
        );
    }
}
