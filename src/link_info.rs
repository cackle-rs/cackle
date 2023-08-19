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
    is_shared: bool,
}

impl LinkInfo {
    pub(crate) fn from_env() -> Result<Self> {
        let crate_sel = CrateSel::from_env()?;
        let object_paths = std::env::args()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| has_supported_extension(path))
            .collect();
        Ok(LinkInfo {
            crate_sel,
            object_paths,
            output_file: get_output_file()?,
            is_shared: get_is_shared(),
        })
    }

    /// Filters `object_paths` to just those under `dir`.
    pub(crate) fn object_paths_under(&self, dir: &Path) -> Vec<PathBuf> {
        self.object_paths
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .filter(|path| path.starts_with(dir))
            .collect()
    }

    /// Returns whether the output of the linker is an executable (not a shared object).
    pub(crate) fn is_executable(&self) -> bool {
        !self.is_shared
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

fn get_is_shared() -> bool {
    std::env::args().any(|arg| arg == "-shared")
}

fn has_supported_extension(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &["rlib", "o"];
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}
