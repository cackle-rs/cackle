use serde::Deserialize;
use serde::Serialize;
use std::fmt::Display;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct SourceLocation {
    pub(crate) filename: PathBuf,
    pub(crate) line: u32,
    pub(crate) column: Option<u32>,
}

impl SourceLocation {
    // Returns whether this source location is from the rust standard library or precompiled crates
    // that are bundled with the standard library (e.g. hashbrown).
    pub(crate) fn is_in_rust_std(&self) -> bool {
        self.filename.starts_with("/rustc/") || self.filename.starts_with("/cargo/registry")
    }
}

impl Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}", self.filename.display(), self.line)?;
        if let Some(column) = self.column {
            write!(f, ":{}", column)?;
        }
        write!(f, "]")
    }
}
