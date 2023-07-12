//! Handles parsing of errors from rustc.

use crate::checker::SourceLocation;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    /// Unsafe was used when it wasn't permitted.
    Unsafe(SourceLocation),
}

/// Looks for known kinds of errors in `output`, which should be the output from rustc with
/// --error-format=json.
pub(crate) fn get_errors(output: &str) -> Vec<ErrorKind> {
    let mut errors = Vec::new();
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<Message>(line) else {
            continue;
        };
        if message.level == "error" && message.code.code == "unsafe_code" {
            if let Some(first_span) = message.spans.first() {
                errors.push(ErrorKind::Unsafe(SourceLocation {
                    filename: PathBuf::from(&first_span.file_name),
                    line: first_span.line_start,
                    column: None,
                }));
            }
        }
    }
    errors
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        assert_eq!(get_errors(""), vec![]);
    }

    #[test]
    fn test_unsafe_error() {
        let json = r#"{
            "code": {"code": "unsafe_code"},
            "level": "error",
            "spans": [
                {
                    "file_name": "src/main.rs",
                    "line_start": 10
                }
            ],
            "rendered": "Stuff that we don't parse"
        }"#
        .replace('\n', "");
        assert_eq!(
            get_errors(&json),
            vec![ErrorKind::Unsafe(SourceLocation {
                filename: PathBuf::from("src/main.rs"),
                line: 10,
                column: None,
            })]
        );
    }
}
