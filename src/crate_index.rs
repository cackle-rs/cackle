//! This module extracts various bits of information from cargo metadata, such as which paths belong
//! to which crates, which are proc macros etc.

use anyhow::Result;
use cargo_metadata::camino::Utf8PathBuf;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use crate::config::CrateName;

#[derive(Default, Debug)]
pub(crate) struct CrateIndex {
    crate_names: HashSet<CrateName>,
    pub(crate) proc_macros: HashSet<CrateName>,
    name_to_dir: HashMap<CrateName, Utf8PathBuf>,
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
}
