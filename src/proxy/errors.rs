//! Handles parsing of errors from rustc.

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    /// Unsafe was used when it wasn't permitted.
    Unsafe(UnsafeUsage),
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub(crate) struct UnsafeUsage {
    pub(crate) file_name: String,
    pub(crate) start_line: u32,
}

/// Looks for a known kind of error in `output`, which should be the output from rustc with
/// --error-format=json.
pub(crate) fn get_error(output: &str) -> Option<ErrorKind> {
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<Message>(line) else { continue };
        if message.level == "error" && message.code.code == "unsafe_code" {
            if let Some(first_span) = message.spans.first() {
                return Some(ErrorKind::Unsafe(UnsafeUsage {
                    file_name: first_span.file_name.clone(),
                    start_line: first_span.line_start,
                }));
            }
        }
    }
    None
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
        assert_eq!(get_error(""), None);
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
        .replace("\n", "");
        assert_eq!(
            get_error(&json),
            Some(ErrorKind::Unsafe(UnsafeUsage {
                file_name: "src/main.rs".to_owned(),
                start_line: 10
            }))
        );
    }
}
