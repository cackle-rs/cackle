use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

/// Information about a linker invocation.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) struct LinkInfo {
    pub(crate) is_build_script: bool,
    pub(crate) package_name: String,
    pub(crate) object_paths: Vec<PathBuf>,
    pub(crate) output_file: PathBuf,
}

impl LinkInfo {
    pub(crate) fn from_env() -> Result<Self> {
        let package_name = std::env::var("CARGO_PKG_NAME").context("CARGO_PKG_NAME not set")?;
        let crate_name = std::env::var("CARGO_CRATE_NAME").context("CARGO_CRATE_NAME not set")?;
        let object_paths = std::env::args()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| has_supported_extension(path))
            .collect();
        Ok(LinkInfo {
            is_build_script: crate_name.starts_with("build_script_"),
            package_name,
            object_paths,
            output_file: get_output_file()?,
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
