//! Code to figure out while source files belong to which crates.

use anyhow::Result;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

#[derive(Default, Debug)]
pub(crate) struct SourceMapping {
    path_to_crate_name: HashMap<PathBuf, String>,
}

impl SourceMapping {
    pub(crate) fn new(dir: &Path) -> Result<Self> {
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(dir.join("Cargo.toml"))
            .exec()?;
        let mut mapping = Self::default();
        for package in metadata.packages {
            for dep in package.dependencies {
                if let Some(path) = dep.path {
                    mapping.path_to_crate_name.insert(path.into(), dep.name);
                }
            }
            for target in package.targets {
                if let Some(target_dir) = target.src_path.into_std_path_buf().parent() {
                    let name = if target.name == "build-script-build" {
                        format!("{}.build", package.name)
                    } else {
                        package.name.clone()
                    };
                    mapping.path_to_crate_name.insert(target_dir.into(), name);
                }
            }
        }
        Ok(mapping)
    }

    pub(crate) fn crate_name_for_path(&self, mut path: &Path) -> Option<&str> {
        loop {
            if let Some(crate_name) = self.path_to_crate_name.get(path) {
                return Some(crate_name);
            }
            if let Some(parent) = path.parent() {
                path = parent;
            } else {
                return None;
            }
        }
    }

    pub(crate) fn crate_names(&self) -> HashSet<&str> {
        self.path_to_crate_name
            .values()
            .map(String::as_str)
            .collect()
    }
}
