use anyhow::Context;
use anyhow::Result;
use std::path::Path;

/// Writes `contents` to `path`. The write is first done to a temporary filename then renamed to
/// `path`. This means that other processes will either see the old contents or the new contents,
/// but should never see a half-written version of the new contents.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, contents)
        .with_context(|| format!("Failed to write `{}`", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename `{}` to `{}`",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}