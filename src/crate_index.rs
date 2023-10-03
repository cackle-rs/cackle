//! This module extracts various bits of information from cargo metadata, such as which paths belong
//! to which crates, which are proc macros etc.

use self::lib_tree::LibTree;
use crate::config::permissions::PermSel;
use crate::config::permissions::PermissionScope;
use crate::config::PackageName;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use cargo_metadata::camino::Utf8PathBuf;
use cargo_metadata::semver::Version;
use cargo_metadata::DependencyKind;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) mod lib_tree;

#[derive(Default, Debug)]
pub(crate) struct CrateIndex {
    pub(crate) manifest_path: PathBuf,
    pub(crate) package_infos: FxHashMap<PackageId, PackageInfo>,
    dir_to_pkg_id: FxHashMap<PathBuf, PackageId>,
    pkg_name_to_ids: FxHashMap<Arc<str>, Vec<PackageId>>,
    lib_tree: LibTree,
    pub(crate) permission_selectors: FxHashSet<PermSel>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PackageId {
    name: Arc<str>,
    version: Version,
    /// Whether this is the only version of this package present in the dependency tree. This is
    /// just used for display purposes. If the name isn't unique, then we display the version as
    /// well.
    name_is_unique: bool,
}

/// Identifies one of several different crates within a package.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CrateSel {
    pub(crate) pkg_id: PackageId,
    pub(crate) kind: CrateKind,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, PartialOrd, Ord)]
pub(crate) enum CrateKind {
    Primary,
    BuildScript,
    Test,
}

#[derive(Debug)]
pub(crate) struct PackageInfo {
    pub(crate) directory: Utf8PathBuf,
    pub(crate) description: Option<String>,
    pub(crate) documentation: Option<String>,
    is_proc_macro: bool,
}

/// The name of the environment variable that we use to pass a list of non-unique package names to
/// our subprocesses. These are packages that have multiple versions present in the output of cargo
/// metadata. Subprocesses need to know which packages are non-unique so that they can correctly
/// form PackageIds, which need this information so that we can only print package versions when
/// there are multiple versions of that package.
pub(crate) const MULTIPLE_VERSION_PKG_NAMES_ENV: &str = "CACKLE_MULTIPLE_VERSION_PKG_NAMES";

impl CrateIndex {
    pub(crate) fn new(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join("Cargo.toml");
        let metadata = cargo_metadata::MetadataCommand::new()
            .manifest_path(&manifest_path)
            .exec()?;
        let mut mapping = CrateIndex {
            manifest_path,
            ..Self::default()
        };
        let mut name_counts = FxHashMap::default();
        for package in &metadata.packages {
            *name_counts.entry(&package.name).or_default() += 1;
        }
        let mut direct_deps: FxHashMap<PackageId, Vec<Arc<str>>> = FxHashMap::default();
        for package in &metadata.packages {
            let pkg_id = PackageId {
                name: Arc::from(package.name.as_str()),
                version: package.version.clone(),
                name_is_unique: name_counts.get(&package.name) == Some(&1),
            };
            let mut is_proc_macro = false;
            let mut has_build_script = false;
            let mut has_test = false;
            for target in &package.targets {
                if target.kind.iter().any(|kind| kind == "proc-macro") {
                    is_proc_macro = true;
                }
                has_build_script |= target.kind.iter().any(|kind| kind == "custom-build");
                has_test |= target.test;
            }
            if let Some(dir) = package.manifest_path.parent() {
                direct_deps.insert(
                    pkg_id.clone(),
                    package
                        .dependencies
                        .iter()
                        .filter(|dep| dep.kind == DependencyKind::Normal && !dep.optional)
                        .map(|dep| Arc::from(dep.name.as_str()))
                        .collect(),
                );
                mapping.package_infos.insert(
                    pkg_id.clone(),
                    PackageInfo {
                        directory: dir.to_path_buf(),
                        description: package.description.clone(),
                        documentation: package.documentation.clone(),
                        is_proc_macro,
                    },
                );
                add_permission_selectors(
                    &mut mapping.permission_selectors,
                    package.name.as_str(),
                    has_build_script,
                    has_test,
                );
                mapping
                    .pkg_name_to_ids
                    .entry(Arc::from(package.name.as_str()))
                    .or_default()
                    .push(pkg_id.clone());
                mapping
                    .dir_to_pkg_id
                    .insert(dir.as_std_path().to_owned(), pkg_id.clone());
            }
        }
        mapping.lib_tree = LibTree::from_workspace(dir, &mapping.pkg_name_to_ids)?;
        for package_ids in mapping.pkg_name_to_ids.values_mut() {
            package_ids.sort_by_key(|pkg_id| pkg_id.version.clone());
        }
        Ok(mapping)
    }

    /// Adds an environment variable to `command` that allows subprocesses to determine whether a
    /// package name is unique.
    pub(crate) fn add_internal_env(&self, command: &mut std::process::Command) {
        let non_unique_names: Vec<&str> = self
            .package_ids()
            .filter_map(|id| {
                if id.name_is_unique {
                    None
                } else {
                    Some(id.name.as_ref())
                }
            })
            .collect();
        command.env(MULTIPLE_VERSION_PKG_NAMES_ENV, non_unique_names.join(","));
    }

    pub(crate) fn newest_package_id_with_name(&self, pkg_name: &PackageName) -> Option<&PackageId> {
        self.pkg_name_to_ids
            .get(pkg_name.as_ref())
            .and_then(|pkg_ids| pkg_ids.last())
    }

    pub(crate) fn package_info(&self, pkg_id: &PackageId) -> Option<&PackageInfo> {
        self.package_infos.get(pkg_id)
    }

    pub(crate) fn pkg_dir(&self, pkg_id: &PackageId) -> Option<&Path> {
        self.package_infos
            .get(pkg_id)
            .map(|info| info.directory.as_std_path())
    }

    pub(crate) fn package_ids(&self) -> impl Iterator<Item = &PackageId> {
        self.package_infos.keys()
    }

    pub(crate) fn proc_macros(&self) -> impl Iterator<Item = &PackageId> {
        self.package_infos.iter().filter_map(|(pkg_id, info)| {
            if info.is_proc_macro {
                Some(pkg_id)
            } else {
                None
            }
        })
    }

    /// Returns the ID of the package that contains the specified path, if any. This is used as a
    /// fallback if we can't locate a source file in the deps emitted by rustc. This can happen for
    /// example in the case of crates that compile C code, since the C code won't be in the deps
    /// file. This function however doesn't differentiate between the build script for a package and
    /// the other source files in that package, so should only be used as a fallback.
    pub(crate) fn package_id_for_path(&self, mut path: &Path) -> Option<&PackageId> {
        loop {
            if let Some(pkg_id) = self.dir_to_pkg_id.get(path) {
                return Some(pkg_id);
            }
            if let Some(parent) = path.parent() {
                path = parent;
            } else {
                return None;
            }
        }
    }

    /// Returns the transitive deps for `pkg_id`. All deps will be in "crate form", i.e. with '-'
    /// replaced with '_'.
    pub(crate) fn transitive_deps(&self, pkg_id: &PackageId) -> Option<&FxHashSet<Arc<str>>> {
        self.lib_tree.pkg_transitive_deps.get(pkg_id)
    }

    /// Returns a map from "crate form" names to package names.
    pub(crate) fn name_prefix_to_pkg_id(&self) -> &FxHashMap<Arc<str>, PackageId> {
        &self.lib_tree.lib_name_to_pkg_id
    }
}

fn add_permission_selectors(
    permission_selectors: &mut FxHashSet<PermSel>,
    pkg_name: &str,
    has_build_script: bool,
    has_test: bool,
) {
    let perm_sel = PermSel::for_primary(pkg_name);
    permission_selectors.insert(perm_sel.clone());
    permission_selectors.insert(perm_sel.clone_with_scope(PermissionScope::FromBuild));
    permission_selectors.insert(perm_sel.clone_with_scope(PermissionScope::FromTest));
    if has_build_script {
        permission_selectors.insert(perm_sel.clone_with_scope(PermissionScope::Build));
    }
    if has_test {
        permission_selectors.insert(perm_sel.clone_with_scope(PermissionScope::Test));
    }
}

impl PackageId {
    pub(crate) fn pkg_name(&self) -> Arc<str> {
        self.name.clone()
    }

    pub(crate) fn from_env() -> Result<Self> {
        let name = get_env("CARGO_PKG_NAME")?;
        let version_string = get_env("CARGO_PKG_VERSION")?;
        let version = Version::parse(&version_string).with_context(|| {
            format!(
                "Package `{}` has invalid version string `{}`",
                name, version_string
            )
        })?;
        let non_unique_pkg_names = get_env(MULTIPLE_VERSION_PKG_NAMES_ENV)?;
        let name_is_unique = non_unique_pkg_names.split(',').all(|p| p != name);

        Ok(PackageId {
            name: Arc::from(name.as_str()),
            version,
            name_is_unique,
        })
    }

    pub(crate) fn version(&self) -> &Version {
        &self.version
    }

    pub(crate) fn crate_name(&self) -> Cow<str> {
        if self.name.contains('-') {
            self.name.replace('-', "_").into()
        } else {
            Cow::Borrowed(&self.name)
        }
    }
}

fn get_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("Failed to get environment variable {key}"))
}

impl CrateSel {
    pub(crate) fn pkg_name(&self) -> Arc<str> {
        self.pkg_id.name.clone()
    }

    pub(crate) fn primary(pkg_id: PackageId) -> Self {
        Self {
            pkg_id,
            kind: CrateKind::Primary,
        }
    }

    pub(crate) fn build_script(pkg_id: PackageId) -> Self {
        Self {
            pkg_id,
            kind: CrateKind::BuildScript,
        }
    }

    pub(crate) fn from_env() -> Result<Self> {
        let pkg_id = PackageId::from_env()?;
        let is_build_script = std::env::var("CARGO_CRATE_NAME")
            .ok()
            .is_some_and(|v| v.starts_with("build_script_"));
        if is_build_script {
            Ok(CrateSel::build_script(pkg_id))
        } else if let Ok(crate_kind) = std::env::var(crate::proxy::subprocess::ENV_CRATE_KIND) {
            CrateSel::primary(pkg_id).with_selector_token(&crate_kind)
        } else {
            Ok(CrateSel::primary(pkg_id))
        }
    }

    pub(crate) fn pkg_id(&self) -> &PackageId {
        &self.pkg_id
    }

    pub(crate) fn selector_token(&self) -> &str {
        self.kind.to_token()
    }

    pub(crate) fn with_selector_token(&self, token: &str) -> Result<Self> {
        Ok(CrateSel {
            pkg_id: self.pkg_id.clone(),
            kind: CrateKind::from_token(token)?,
        })
    }
}

impl CrateKind {
    fn to_token(self) -> &'static str {
        match self {
            CrateKind::Primary => "primary",
            CrateKind::BuildScript => "build-script",
            CrateKind::Test => "test",
        }
    }

    fn from_token(token: &str) -> Result<Self> {
        Ok(match token {
            "primary" => CrateKind::Primary,
            "build-script" => CrateKind::BuildScript,
            "test" => CrateKind::Test,
            other => bail!("Invalid crate selector token `{other}`"),
        })
    }
}

impl From<&PackageId> for PackageName {
    fn from(pkg_id: &PackageId) -> Self {
        PackageName(pkg_id.name.clone())
    }
}

impl Display for CrateSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.pkg_id.name)?;
        match self.kind {
            CrateKind::BuildScript => write!(f, ".build")?,
            CrateKind::Primary => {}
            CrateKind::Test => write!(f, ".test")?,
        }
        if !self.pkg_id.name_is_unique {
            write!(f, "[{}]", self.pkg_id.version)?;
        }
        Ok(())
    }
}

impl Display for PackageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        CrateSel::primary(self.clone()).fmt(f)
    }
}

impl PackageId {
    pub(crate) fn name_str(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::CrateIndex;
    use super::PackageId;
    use super::PackageInfo;
    use cargo_metadata::semver::Version;
    use fxhash::FxHashSet;
    use std::sync::Arc;

    pub(crate) fn pkg_id(name: &str) -> PackageId {
        PackageId {
            name: Arc::from(name),
            version: Version::new(0, 0, 0),
            name_is_unique: true,
        }
    }

    pub(crate) fn index_with_package_names(package_names: &[&str]) -> Arc<CrateIndex> {
        let package_infos = package_names
            .iter()
            .map(|name| {
                (
                    pkg_id(name),
                    PackageInfo {
                        directory: Default::default(),
                        description: Default::default(),
                        documentation: Default::default(),
                        is_proc_macro: Default::default(),
                    },
                )
            })
            .collect();
        let mut permission_selectors = FxHashSet::default();
        for pkg_name in package_names {
            super::add_permission_selectors(&mut permission_selectors, pkg_name, false, false);
        }
        Arc::new(CrateIndex {
            package_infos,
            permission_selectors,
            ..CrateIndex::default()
        })
    }
}

#[test]
fn test_crate_index() {
    #[track_caller]
    fn check(index: &CrateIndex, from: &str, expected_deps: &[&str]) {
        let Some(pkg_id) = index.name_prefix_to_pkg_id().get(from) else {
            panic!("Missing package ID for `{from}`");
        };
        let Some(deps) = index.transitive_deps(pkg_id) else {
            panic!("No deps for {pkg_id}");
        };
        let mut sorted_deps: Vec<&str> = deps.iter().map(|d| d.as_ref()).collect();
        sorted_deps.sort();
        assert_eq!(sorted_deps, expected_deps);
    }

    let crate_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let test_crates_dir = crate_root.join("test_crates");
    let index = CrateIndex::new(&test_crates_dir).unwrap();

    check(&index, "crab_2", &["crab_1", "crab_3"]);
    check(&index, "crab_4", &[]);
    check(
        &index,
        "crab_bin",
        &[
            "crab_1", "crab_2", "crab_3", "crab_4", "crab_5", "crab_6", "crab_7", "crab_8", "res_1",
        ],
    );
}
