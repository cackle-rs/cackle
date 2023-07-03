//! This module extracts various bits of information from cargo metadata, such as which paths belong
//! to which crates, which are proc macros etc.

use anyhow::Result;
use cargo_metadata::camino::Utf8PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

#[derive(Default, Debug)]
pub(crate) struct CrateIndex {
    crate_names: HashSet<Box<str>>,
    pub(crate) proc_macros: HashSet<String>,
    name_to_dir: HashMap<String, Utf8PathBuf>,
}

impl CrateIndex {
    pub(crate) fn new(dir: &Path) -> Result<Self> {
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(dir.join("Cargo.toml"))
            .exec()?;
        let mut mapping = Self::default();
        for package in metadata.packages {
            for target in package.targets {
                if target.name.starts_with("build-script-") {
                    mapping
                        .crate_names
                        .insert(format!("{}.build", package.name).into_boxed_str());
                };
                if target.kind.iter().any(|kind| kind == "proc-macro") {
                    mapping.proc_macros.insert(package.name.clone());
                }
            }
            if let Some(dir) = package.manifest_path.parent() {
                mapping
                    .name_to_dir
                    .insert(package.name.clone(), dir.to_path_buf());
            }
            mapping.crate_names.insert(package.name.into_boxed_str());
        }
        Ok(mapping)
    }

    pub(crate) fn pkg_dir(&self, pkg_name: &str) -> Option<&Utf8PathBuf> {
        self.name_to_dir.get(pkg_name)
    }

    pub(crate) fn package_names(&self) -> impl Iterator<Item = &str> {
        self.name_to_dir.keys().map(String::as_str)
    }

    pub(crate) fn crate_names(&self) -> impl Iterator<Item = &str> {
        self.crate_names.iter().map(|n| n.as_ref())
    }
}
