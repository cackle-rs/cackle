//! This module extracts various bits of information from cargo metadata, such as which paths belong
//! to which crates, which are proc macros etc.

use anyhow::Result;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;

#[derive(Default, Debug)]
pub(crate) struct CrateIndex {
    path_to_crate_name: HashMap<PathBuf, String>,
    pub(crate) proc_macros: HashSet<String>,
}

impl CrateIndex {
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
                if target.kind.iter().any(|kind| kind == "proc-macro") {
                    mapping.proc_macros.insert(package.name.clone());
                }
            }
        }
        Ok(mapping)
    }

    pub(crate) fn crate_name_for_path(&self, mut path: &Path) -> Option<&str> {
        let x = path;
        loop {
            if let Some(crate_name) = self.path_to_crate_name.get(path) {
                return Some(crate_name);
            }
            if let Some(parent) = path.parent() {
                path = parent;
            } else {
                println!("No crate name for {}", x.display());
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

impl Display for CrateIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (path, name) in &self.path_to_crate_name {
            writeln!(f, "{} -> {name}", path.display())?;
        }
        Ok(())
    }
}
