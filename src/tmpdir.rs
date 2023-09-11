use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

pub(crate) enum TempDir {
    Owned(tempfile::TempDir),
    Borrowed(PathBuf),
}

impl TempDir {
    pub(crate) fn new(path: Option<&Path>) -> Result<Self> {
        if let Some(path) = path {
            Ok(TempDir::Borrowed(path.to_owned()))
        } else {
            Ok(TempDir::Owned(tempfile::TempDir::new()?))
        }
    }

    pub(crate) fn path(&self) -> &Path {
        match self {
            TempDir::Owned(t) => t.path(),
            TempDir::Borrowed(t) => t,
        }
    }
}
