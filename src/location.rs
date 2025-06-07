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

    pub(crate) fn filename(&self) -> &Path {
        &self.filename
    }

    pub(crate) fn line(&self) -> u32 {
        self.line
    }

    pub(crate) fn column(&self) -> Option<u32> {
        self.column
    }

    pub(crate) fn with_sysroot(&self, sysroot: &Path) -> Self {
        if !self.filename.starts_with("/rustc/") {
            return self.clone();
        }
        let mut filename = sysroot.join("lib/rustlib/src/rust");
        filename.extend(self.filename.iter().skip(3));
        Self {
            filename: Arc::from(filename.as_path()),
            line: self.line,
            column: self.column,
        }
    }
}

impl Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}", self.filename.display(), self.line)?;
        if let Some(column) = self.column {
            write!(f, ":{column}")?;
        }
        write!(f, "]")
    }
}
