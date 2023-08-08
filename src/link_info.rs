use anyhow::bail;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

use crate::crate_index::CrateSel;

/// Information about a linker invocation.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) struct LinkInfo {
    pub(crate) crate_sel: CrateSel,
    pub(crate) object_paths: Vec<PathBuf>,
    pub(crate) output_file: PathBuf,
}

impl LinkInfo {
    pub(crate) fn from_env() -> Result<Self> {
        let crate_sel = CrateSel::from_env()?;
        // We only examine objects files that under the current directory. This avoids us needing to
        // process the rust standard library.
        let current_dir = std::env::current_dir()?;
        let object_paths = std::env::args()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| has_supported_extension(path) && path.starts_with(&current_dir))
            .collect();
        Ok(LinkInfo {
            crate_sel,
            object_paths,
            output_file: get_output_file()?,
        })
    }
    pub(crate) fn is_build_script(&self) -> bool {
        matches!(self.crate_sel, CrateSel::BuildScript(_))
    }
}

fn get_output_file() -> Result<PathBuf> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == "-o" {
            if let Some(output) = args.next() {
                return Ok(PathBuf::from(output));
            }
        }
    }
    bail!("Failed to find output file in linker command line");
}

fn has_supported_extension(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &["rlib", "o"];
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}
