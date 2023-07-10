//! This module extracts various bits of information from cargo metadata, such as which paths belong
//! to which crates, which are proc macros etc.

use anyhow::Result;
use cargo_metadata::camino::Utf8PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use crate::config::CrateName;

#[derive(Default, Debug)]
pub(crate) struct CrateIndex {
    crate_names: HashSet<CrateName>,
    pub(crate) proc_macros: HashSet<CrateName>,
    name_to_dir: HashMap<CrateName, Utf8PathBuf>,
    dir_to_name: HashMap<PathBuf, CrateName>,
}

impl CrateIndex {
    pub(crate) fn new(dir: &Path) -> Result<Self> {
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(dir.join("Cargo.toml"))
            .exec()?;
        let mut mapping = Self::default();
        for package in metadata.packages {
            let crate_name = CrateName::from(package.name.as_str());
            for target in package.targets {
                if target.name.starts_with("build-script-") {
                    mapping
                        .crate_names
                        .insert(CrateName::for_build_script(&package.name));
                };
                if target.kind.iter().any(|kind| kind == "proc-macro") {
                    mapping.proc_macros.insert(crate_name.clone());
                }
            }
            if let Some(dir) = package.manifest_path.parent() {
                mapping
                    .name_to_dir
                    .insert(crate_name.clone(), dir.to_path_buf());
                mapping
                    .dir_to_name
                    .insert(dir.as_std_path().to_owned(), crate_name.clone());
            }
            mapping.crate_names.insert(crate_name);
        }
        Ok(mapping)
    }

    pub(crate) fn pkg_dir(&self, crate_name: &CrateName) -> Option<&Utf8PathBuf> {
        self.name_to_dir.get(crate_name)
    }

    pub(crate) fn package_names(&self) -> impl Iterator<Item = &CrateName> {
        self.name_to_dir.keys()
    }

    pub(crate) fn crate_names(&self) -> impl Iterator<Item = &CrateName> {
        self.crate_names.iter()
    }

    /// Returns the name of the crate that contains the specified path, if any. This is used as a
    /// fallback if we can't locate a source file in the deps emitted by rustc. This can happen for
    /// example in the case of crates that compile C code, since the C code won't be in the deps
    /// file. This function however doesn't differentiate between the build script for a package and
    /// the other source files in that package, so should only be used as a fallback.
    pub(crate) fn crate_name_for_path(&self, mut path: &Path) -> Option<&CrateName> {
        loop {
            if let Some(crate_name) = self.dir_to_name.get(path) {
                return Some(crate_name);
            }
            if let Some(parent) = path.parent() {
                path = parent;
            } else {
                return None;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::CrateIndex;
    use crate::config::CrateName;
    use std::sync::Arc;

    pub(crate) fn index_with_crate_names(crate_names: &[&str]) -> Arc<CrateIndex> {
        let crate_names = crate_names
            .iter()
            .map(|name| CrateName(Arc::from(*name)))
            .collect();
        Arc::new(CrateIndex {
            crate_names,
            ..CrateIndex::default()
        })
    }
}
