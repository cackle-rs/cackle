use serde::Deserialize;
use serde::Serialize;
use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct SourceLocation {
    filename: Arc<Path>,
    line: u32,
    column: Option<u32>,
}

impl SourceLocation {
    pub(crate) fn new<P: Into<Arc<Path>>>(filename: P, line: u32, column: Option<u32>) -> Self {
        Self {
            filename: filename.into(),
            line,
            column,
        }
    }

    // Returns whether this source location is from the rust standard library or precompiled crates
    // that are bundled with the standard library (e.g. hashbrown).
    pub(crate) fn is_in_rust_std(&self) -> bool {
        self.filename.starts_with("/rustc/") || self.filename.starts_with("/cargo/registry")
    }

    pub(crate) fn filename(&self) -> &Path {
        &self.filename
    }

    pub(crate) fn line(&self) -> u32 {
        self.line
    }

    pub(crate) fn column(&self) -> Option<u32> {
        self.column
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
