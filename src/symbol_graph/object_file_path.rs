use anyhow::Context;
use anyhow::Result;
use std::fmt::Display;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;

/// Represents the name of an object file, possibly contained within an archive. Note, we only
/// support a single level of archive. i.e. archives within archives aren't supported.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ObjectFilePath {
    pub(crate) outer: PathBuf,
    pub(crate) inner: Option<PathBuf>,
}

impl ObjectFilePath {
    pub(crate) fn non_archive(filename: &Path) -> Self {
        Self {
            outer: filename.to_owned(),
            inner: None,
        }
    }

    pub(crate) fn in_archive(archive: &Path, entry: &ar::Entry<File>) -> Result<Self> {
        let inner = PathBuf::from(
            std::str::from_utf8(entry.header().identifier()).with_context(|| {
                format!(
                    "An archive entry in `{}` is not valid UTF-8",
                    archive.display()
                )
            })?,
        );
        Ok(Self {
            outer: archive.to_owned(),
            inner: Some(inner),
        })
    }
}

impl Display for ObjectFilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(inner) = self.inner.as_ref() {
            write!(f, "{}[{}]", self.outer.display(), inner.display())
        } else {
            write!(f, "{}", self.outer.display())
        }
    }
}
