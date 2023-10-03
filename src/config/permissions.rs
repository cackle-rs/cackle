use super::PackageConfig;
use super::PackageName;
use super::RawConfig;
use super::SandboxConfig;
use crate::crate_index::CrateIndex;
use crate::crate_index::CrateKind;
use crate::crate_index::CrateSel;
use crate::crate_index::PackageId;
use anyhow::Result;
use fxhash::FxHashMap;
use serde::Deserialize;
use serde::Serialize;
use std::fmt::Display;
use std::sync::Arc;

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Permissions {
    pub(crate) packages: FxHashMap<PermSel, PackageConfig>,
}

/// A permission selector. Identifies a group of permissions.
#[derive(Debug, Hash, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub(crate) struct PermSel {
    pub(crate) package_name: PackageName,
    pub(crate) scope: PermissionScope,
}

/// Determines the scope of a permission with respect to a particular package.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy, PartialOrd, Ord, Deserialize, Serialize)]
pub(crate) enum PermissionScope {
    /// Permission is granted to the package regardless of what binary it's used from.
    All,
    /// Permission is granted to the build script of the specified package.
    Build,
    /// Permission is granted to tests of the specified package.
    Test,
    /// Permission is granted to the package, but only when used via build scripts of other
    /// packages.
    FromBuild,
    /// Permission is granted to the package, but only when used via tests of other packages.
    FromTest,
}

impl Permissions {
    pub(crate) fn from_config(config: &RawConfig) -> Self {
        let mut packages = FxHashMap::default();
        for (name, pkg_config) in &config.packages {
            let mut pkg_config = pkg_config.clone();
            if let Some(sub_cfg) = pkg_config.build.take() {
                packages.insert(
                    PermSel {
                        package_name: name.clone(),
                        scope: PermissionScope::Build,
                    },
                    *sub_cfg,
                );
            }
            if let Some(sub_cfg) = pkg_config.test.take() {
                packages.insert(
                    PermSel {
                        package_name: name.clone(),
                        scope: PermissionScope::Test,
                    },
                    *sub_cfg,
                );
            }
            if let Some(mut dep) = pkg_config.from.take() {
                if let Some(sub_cfg) = dep.build.take() {
                    packages.insert(
                        PermSel {
                            package_name: name.clone(),
                            scope: PermissionScope::FromBuild,
                        },
                        *sub_cfg,
                    );
                }
                if let Some(sub_cfg) = dep.test.take() {
                    packages.insert(
                        PermSel {
                            package_name: name.clone(),
                            scope: PermissionScope::FromTest,
                        },
                        *sub_cfg,
                    );
                }
            }
            packages.insert(
                PermSel {
                    package_name: name.clone(),
                    scope: PermissionScope::All,
                },
                pkg_config,
            );
        }
        Self { packages }
    }

    pub(crate) fn from_config_with_inheritance(
        config: &RawConfig,
        crate_index: &CrateIndex,
    ) -> Self {
        let mut new = Self::from_config(config);
        for sel in &crate_index.permission_selectors {
            new.packages.entry(sel.clone()).or_default();
        }
        apply_inheritance(&mut new.packages, config);
        new
    }

    pub(crate) fn sandbox_config_for_package(&self, perm_sel: &PermSel) -> SandboxConfig {
        self.packages
            .get(perm_sel)
            .map(|c| c.sandbox.clone())
            .unwrap_or_default()
    }

    pub(crate) fn unsafe_permitted_for_crate(&self, crate_sel: &CrateSel) -> bool {
        self.packages
            .get(&PermSel::for_non_build_output(crate_sel))
            .is_some_and(|crate_config| crate_config.allow_unsafe)
    }

    pub(crate) fn get(&self, perm_sel: &PermSel) -> Option<&PackageConfig> {
        self.packages.get(perm_sel)
    }
}

fn apply_inheritance(packages: &mut FxHashMap<PermSel, PackageConfig>, config: &RawConfig) {
    // Determine a global config. We may eventually make this an actual thing in our configuration
    // file.
    let global_config = PackageConfig {
        sandbox: config.sandbox.clone(),
        ..Default::default()
    };

    // Separate out the configs into a map per layer. Note, we move everything out of `packages`,
    // then put them back later.
    let mut all = FxHashMap::default();
    let mut dep = FxHashMap::default();
    let mut local = FxHashMap::default();
    for (perm_sel, config) in std::mem::take(packages) {
        match perm_sel.scope {
            PermissionScope::All => all.insert(perm_sel, config),
            PermissionScope::Build => local.insert(perm_sel, config),
            PermissionScope::Test => local.insert(perm_sel, config),
            PermissionScope::FromBuild => dep.insert(perm_sel, config),
            PermissionScope::FromTest => dep.insert(perm_sel, config),
        };
    }

    // Apply inheritance between the layers
    for config in all.values_mut() {
        config.inherit(&global_config);
    }
    for (perm_sel, config) in dep.iter_mut() {
        if let Some(parent) = all.get(&perm_sel.clone_with_scope(PermissionScope::All)) {
            config.inherit(parent);
        }
    }
    for (perm_sel, config) in local.iter_mut() {
        let parent_scope = match perm_sel.scope {
            PermissionScope::Build => PermissionScope::FromBuild,
            PermissionScope::Test => PermissionScope::FromTest,
            _ => unreachable!(),
        };
        if let Some(parent) = dep.get(&perm_sel.clone_with_scope(parent_scope)) {
            config.inherit(parent);
        }
    }

    // Recombine the layers back into the original map.
    packages.extend(all);
    packages.extend(dep);
    packages.extend(local);
}

impl PackageConfig {
    fn inherit(&mut self, other: &PackageConfig) {
        merge_string_vec(&mut self.allow_apis, &other.allow_apis);
        merge_string_vec(
            &mut self.allow_build_instructions,
            &other.allow_build_instructions,
        );
        self.allow_proc_macro |= other.allow_proc_macro;
        self.allow_unsafe |= other.allow_unsafe;
        self.sandbox.inherit(&other.sandbox);
    }
}

impl SandboxConfig {
    pub(crate) fn inherit(&mut self, other: &SandboxConfig) {
        if self.kind.is_none() {
            self.kind = other.kind;
        }
        merge_string_vec(&mut self.extra_args, &other.extra_args);
        merge_string_vec(&mut self.bind_writable, &other.bind_writable);
        merge_string_vec(&mut self.make_writable, &other.make_writable);
        if self.allow_network.is_none() {
            self.allow_network = other.allow_network;
        }
    }
}

fn merge_string_vec<T: Ord + Clone>(add_to: &mut Vec<T>, add: &[T]) {
    add_to.extend_from_slice(add);
    add_to.sort();
    add_to.dedup();
}

impl PermSel {
    pub(crate) fn with_scope(use_package: &PackageId, scope: PermissionScope) -> Self {
        Self {
            package_name: PackageName(use_package.pkg_name()),
            scope,
        }
    }

    /// Converts a crate selector to a permission selector. This conversion is only appropriate when
    /// the usage isn't attributed to a particular build output. e.g. it's fine for creating a
    /// permission selector for unsafe usage, but not for API usage. For API usage, we should always
    /// take into account what kind of build we're doing i.e. use `with_scope`.
    pub(crate) fn for_non_build_output(crate_sel: &CrateSel) -> Self {
        let scope = match crate_sel.kind {
            CrateKind::Primary => PermissionScope::All,
            CrateKind::BuildScript => PermissionScope::Build,
            CrateKind::Test => PermissionScope::Test,
        };
        Self::with_scope(&crate_sel.pkg_id, scope)
    }

    pub(crate) fn for_primary<N: Into<Arc<str>>>(pkg_name: N) -> Self {
        Self {
            package_name: PackageName(pkg_name.into()),
            scope: PermissionScope::All,
        }
    }

    pub(crate) fn for_build_script<N: Into<Arc<str>>>(pkg_name: N) -> Self {
        Self {
            package_name: PackageName(pkg_name.into()),
            scope: PermissionScope::Build,
        }
    }

    pub(crate) fn clone_with_scope(&self, scope: PermissionScope) -> Self {
        Self {
            package_name: self.package_name.clone(),
            scope,
        }
    }

    pub(crate) fn parent(&self) -> Option<PermSel> {
        Some(self.clone_with_scope(self.scope.parent_scope()?))
    }

    /// Returns all selectors that inherit from this one.
    pub(crate) fn descendants(&self) -> Vec<PermSel> {
        let mut scopes: Vec<PermSel> = self
            .scope
            .child_scopes()
            .iter()
            .map(|s| self.clone_with_scope(*s))
            .collect();
        let mut next_level: Vec<PermSel> = scopes
            .iter()
            .flat_map(|sel| {
                sel.scope
                    .child_scopes()
                    .iter()
                    .map(|s| self.clone_with_scope(*s))
            })
            .collect();
        scopes.append(&mut next_level);
        scopes
    }
}

impl Display for PermSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.package_name.fmt(f)?;
        if let Some(kind_str) = self.scope.config_selector() {
            '.'.fmt(f)?;
            kind_str.fmt(f)?;
        }
        Ok(())
    }
}

impl PermissionScope {
    pub(crate) fn config_selector(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Build => Some("build"),
            Self::Test => Some("test"),
            Self::FromBuild => Some("from.build"),
            Self::FromTest => Some("from.test"),
        }
    }

    pub(crate) fn determine(use_pkg: &PackageId, bin_selector: &CrateSel) -> PermissionScope {
        if use_pkg == &bin_selector.pkg_id {
            match bin_selector.kind {
                CrateKind::Primary => PermissionScope::All,
                CrateKind::BuildScript => PermissionScope::Build,
                CrateKind::Test => PermissionScope::Test,
            }
        } else {
            match bin_selector.kind {
                CrateKind::Primary => PermissionScope::All,
                CrateKind::BuildScript => PermissionScope::FromBuild,
                CrateKind::Test => PermissionScope::FromTest,
            }
        }
    }

    pub(crate) fn parent_scope(self) -> Option<PermissionScope> {
        match self {
            PermissionScope::All => None,
            PermissionScope::Build => Some(PermissionScope::FromBuild),
            PermissionScope::Test => Some(PermissionScope::FromTest),
            PermissionScope::FromBuild => Some(PermissionScope::All),
            PermissionScope::FromTest => Some(PermissionScope::All),
        }
    }

    fn child_scopes(self) -> &'static [PermissionScope] {
        match self {
            PermissionScope::All => &[PermissionScope::FromBuild, PermissionScope::FromTest],
            PermissionScope::Build => &[],
            PermissionScope::Test => &[],
            PermissionScope::FromBuild => &[PermissionScope::Build],
            PermissionScope::FromTest => &[PermissionScope::Test],
        }
    }
}

/// A manual implementation of Serialize for PermSel so that we can use it as keys in a hashmap that
/// gets serialised.
impl Serialize for PermSel {
    fn serialize<S>(&self, serialiser: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // We do the simplest thing we can, which is to just put our fields into a tuple, then
        // serialise that as JSON.
        let to_serialise = (self.package_name.clone(), self.scope);
        serialiser.serialize_str(&serde_json::to_string(&to_serialise).unwrap())
    }
}

/// A manual implementation of Serialize for PermSel so that we can use it as keys in a hashmap that
/// gets serialised.
impl<'de> Deserialize<'de> for PermSel {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserialiser: D,
    ) -> std::result::Result<Self, D::Error> {
        deserialiser.deserialize_str(PermSelVisitor)
    }
}

struct PermSelVisitor;

impl<'de> serde::de::Visitor<'de> for PermSelVisitor {
    type Value = PermSel;

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let (package_name, scope) = serde_json::from_str(s)
            .map_err(|_| serde::de::Error::invalid_value(serde::de::Unexpected::Str(s), &self))?;
        Ok(PermSel {
            package_name,
            scope,
        })
    }

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("the fields of a PermSel encoded as a JSON array")
    }
}

#[test]
fn test_inheritance() {
    fn parse_config(
        crate_index: &CrateIndex,
        cackle: &str,
    ) -> anyhow::Result<Arc<crate::config::Config>> {
        let raw = super::parse_raw(cackle)?;
        crate::config::Config::from_raw(raw, crate_index)
    }

    let bar1 = PermSel::for_primary("bar1");
    let bar1_dep_test = bar1.clone_with_scope(PermissionScope::FromTest);
    let mut crate_index = CrateIndex::default();
    crate_index
        .permission_selectors
        .insert(bar1_dep_test.clone());

    let config = parse_config(
        &crate_index,
        r#"
        [common]
        version = 1
        import_std = ["fs", "process"]

        [pkg.bar1]
        allow_unsafe = true
        allow_apis = [
            "fs",
            "process",
        ]

        [pkg.bar1.test]
    "#,
    )
    .unwrap();

    let bar1_dep_test_config = config.permissions.get(&bar1_dep_test).unwrap();
    assert!(bar1_dep_test_config.allow_unsafe);
    let bar1_test_config = config
        .permissions
        .get(&bar1.clone_with_scope(PermissionScope::Test))
        .unwrap();
    assert!(bar1_test_config.allow_unsafe);
    assert_eq!(bar1_test_config.allow_apis, &["fs", "process"])
}
