use crate::crate_index::CrateIndex;
use crate::crate_index::CrateKind;
use crate::crate_index::CrateSel;
use crate::crate_index::PackageId;
use crate::problem::AvailableApi;
use crate::problem::Problem;
use crate::problem::ProblemList;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use fxhash::FxHashMap;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) mod built_in;

pub(crate) const MAX_VERSION: i64 = 1;

#[derive(Default, Debug)]
pub(crate) struct Config {
    pub(crate) raw: RawConfig,

    pub(crate) permissions: FxHashMap<PermSel, PackageConfig>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    pub(crate) common: CommonConfig,

    #[serde(default, rename = "api")]
    pub(crate) apis: BTreeMap<ApiName, ApiConfig>,

    #[serde(default, rename = "pkg")]
    packages: BTreeMap<PackageName, PackageConfig>,

    #[serde(default)]
    pub(crate) sandbox: SandboxConfig,
}

/// The name of a package. Doesn't include any version information.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub(crate) struct PackageName(pub(crate) Arc<str>);

/// A permission selector. Identifies a group of permissions.
#[derive(Debug, Hash, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub(crate) struct PermSel {
    pub(crate) package_name: PackageName,
    pub(crate) kind: CrateKind,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommonConfig {
    pub(crate) version: i64,

    #[serde(default)]
    pub(crate) explicit_build_scripts: bool,

    #[serde(default)]
    pub(crate) build_flags: Option<Vec<String>>,

    #[serde(default)]
    pub(crate) import_std: Vec<String>,

    #[serde(default)]
    pub(crate) features: Vec<String>,

    #[serde(default)]
    pub(crate) profile: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxConfig {
    #[serde(default)]
    pub(crate) kind: SandboxKind,

    #[serde(default)]
    pub(crate) extra_args: Vec<String>,

    pub(crate) allow_network: Option<bool>,

    #[serde(default)]
    pub(crate) bind_writable: Vec<PathBuf>,

    #[serde(default)]
    pub(crate) make_writable: Vec<PathBuf>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApiConfig {
    #[serde(default)]
    pub(crate) include: Vec<ApiPath>,

    #[serde(default)]
    pub(crate) exclude: Vec<ApiPath>,

    #[serde(default)]
    pub(crate) no_auto_detect: Vec<PackageName>,
}

#[derive(Deserialize, Serialize, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone)]
#[serde(transparent)]
pub(crate) struct ApiName {
    pub(crate) name: Arc<str>,
}

/// A path prefix to some API. e.g. `std::net`.
#[derive(Deserialize, Serialize, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone)]
#[serde(transparent)]
pub(crate) struct ApiPath {
    pub(crate) prefix: Arc<str>,
}

#[derive(Deserialize, Serialize, Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SandboxKind {
    #[default]
    Inherit,
    Disabled,
    Bubblewrap,
}

pub(crate) const SANDBOX_KINDS: &[SandboxKind] = &[SandboxKind::Disabled, SandboxKind::Bubblewrap];

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageConfig {
    #[serde(default)]
    pub(crate) allow_unsafe: bool,

    #[serde(default)]
    pub(crate) allow_build_instructions: Vec<String>,

    #[serde(default)]
    pub(crate) allow_apis: Vec<ApiName>,

    #[serde(default)]
    pub(crate) allow_proc_macro: bool,

    /// Configuration for this crate's build.rs. Only used during parsing, after which it's
    /// flattened out.
    build: Option<Box<PackageConfig>>,

    /// Configuration for this crate's tests. Only used during parsing, after which it's flattened
    /// out.
    test: Option<Box<PackageConfig>>,

    #[serde()]
    pub(crate) sandbox: Option<SandboxConfig>,

    #[serde(default)]
    pub(crate) import: Option<Vec<String>>,
}

pub(crate) fn parse_file(cackle_path: &Path, crate_index: &CrateIndex) -> Result<Arc<Config>> {
    let mut raw_config = parse_file_raw(cackle_path)?;
    raw_config.load_imports(crate_index)?;
    raw_config.make_paths_absolute(crate_index.manifest_path.parent())?;
    let permissions = permission_map(&raw_config);
    let config = Config {
        raw: raw_config,
        permissions,
    };
    crate::config_validation::validate(&config, cackle_path)?;
    Ok(Arc::new(config))
}

fn parse_file_raw(cackle_path: &Path) -> Result<RawConfig> {
    let cackle: String = std::fs::read_to_string(cackle_path)
        .with_context(|| format!("Failed to open {}", cackle_path.display()))?;
    let raw_config =
        parse_raw(&cackle).with_context(|| format!("Failed to parse {}", cackle_path.display()))?;
    Ok(raw_config)
}

fn parse_raw(cackle: &str) -> Result<RawConfig> {
    let mut config = toml::from_str(cackle)?;
    merge_built_ins(&mut config)?;
    Ok(config)
}

fn merge_built_ins(config: &mut RawConfig) -> Result<()> {
    if config.common.import_std.is_empty() {
        return Ok(());
    }
    let built_ins = built_in::get_built_ins();
    for imp in config.common.import_std.drain(..) {
        let api = ApiName::new(imp.as_str());
        let built_in_api = built_ins
            .get(&api)
            .ok_or_else(|| anyhow!("Unknown API `{imp}` in import_std"))?;
        let api_config = config.apis.entry(api).or_insert_with(Default::default);
        api_config
            .include
            .extend(built_in_api.include.iter().cloned());
        api_config
            .exclude
            .extend(built_in_api.exclude.iter().cloned());
    }
    Ok(())
}

impl RawConfig {
    fn load_imports(&mut self, crate_index: &CrateIndex) -> Result<()> {
        for (pkg_name, pkg_config) in &mut self.packages {
            // If imports are specified, then we leave an empty list of imports. This ensures that
            // later in unused_imports, we can determine whether each package specified imports or
            // not. Although we leave an empty Vec, we can't leave a non-empty Vec, because then
            // that would get written into the flattened config that gets read by subprocesses and
            // those can't handle loading imports because they don't run `cargo metadata`.
            let mut imports = Vec::new();
            if let Some(i) = pkg_config.import.as_mut() {
                std::mem::swap(i, &mut imports);
            }
            if imports.is_empty() {
                continue;
            }
            let pkg_id = crate_index
                .newest_package_id_with_name(pkg_name)
                .ok_or_else(|| {
                    anyhow!("Attempted to import APIs from package `{pkg_name}` that wasn't found")
                })?;
            let pkg_exports = exported_config_for_package(pkg_id, crate_index)?;
            for (api_name, api_def) in &pkg_exports.apis {
                if !imports.iter().any(|imp| imp == api_name.name.as_ref()) {
                    // The user didn't request importing this API, so skip it.
                    continue;
                }
                let qualified_api_name = ApiName {
                    name: format!("{pkg_name}::{api_name}").into(),
                };
                if self
                    .apis
                    .insert(qualified_api_name.clone(), api_def.clone())
                    .is_some()
                {
                    bail!("[pkg.{pkg_name}.api.{api_name}] is defined multiple times");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn toml_string(&self) -> Result<String> {
        Ok(toml::to_string(self)?)
    }

    /// Return warnings for all packages that export APIs but where we have no import for that
    /// package. Users can suppress this warning by either importing an API, or if they don't want
    /// to import any APIs from this package, by listing `import = []`.
    pub(crate) fn unused_imports(&self, crate_index: &CrateIndex) -> ProblemList {
        let mut problems = ProblemList::default();
        for pkg_id in crate_index.package_ids() {
            // If our config lists any import for this package, even empty, then we skip this.
            if self
                .packages
                .get(&PackageName(Arc::from(pkg_id.name())))
                .map(|config| config.import.is_some())
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(pkg_exports) = exported_config_for_package(pkg_id, crate_index) else {
                continue;
            };
            for (api, config) in &pkg_exports.apis {
                problems.push(Problem::AvailableApi(AvailableApi {
                    pkg_id: pkg_id.clone(),
                    api: api.clone(),
                    config: config.clone(),
                }))
            }
        }
        problems
    }
}

impl RawConfig {
    fn make_paths_absolute(&mut self, workspace_root: Option<&Path>) -> Result<()> {
        for pkg_config in self.packages.values_mut() {
            pkg_config.make_paths_absolute(workspace_root)?;
        }
        Ok(())
    }
}

impl PackageConfig {
    fn make_paths_absolute(&mut self, workspace_root: Option<&Path>) -> Result<()> {
        if let Some(sandbox_config) = self.sandbox.as_mut() {
            sandbox_config.make_paths_absolute(workspace_root)?;
        }
        if let Some(sub_config) = self.build.as_mut() {
            sub_config.make_paths_absolute(workspace_root)?;
        }
        if let Some(sub_config) = self.test.as_mut() {
            sub_config.make_paths_absolute(workspace_root)?;
        }
        Ok(())
    }
}

impl SandboxConfig {
    fn make_paths_absolute(&mut self, workspace_root: Option<&Path>) -> Result<()> {
        make_paths_absolute(&mut self.bind_writable, workspace_root)?;
        make_paths_absolute(&mut self.make_writable, workspace_root)?;
        Ok(())
    }
}

fn make_paths_absolute(paths: &mut [PathBuf], workspace_root: Option<&Path>) -> Result<()> {
    for path in paths {
        if !path.is_absolute() {
            // When we process the config file in the main cackle process, we should
            // always have a workspace root. At that point all paths should be made
            // absolute. Subprocesses won't know the workspace root, but the paths
            // should already be absolute, since they should be reading a processed
            // version of the config written by the main process.
            let workspace_root = workspace_root
                .ok_or_else(|| anyhow!("Internal error: relative path with no workspace root"))?;
            *path = workspace_root.join(&path);
        }
    }
    Ok(())
}

/// Attempts to load "cackle/export.toml" from the specified package.
fn exported_config_for_package(pkg_id: &PackageId, crate_index: &CrateIndex) -> Result<RawConfig> {
    let pkg_dir = crate_index
        .pkg_dir(pkg_id)
        .ok_or_else(|| anyhow!("Missing pkg_dir for package `{pkg_id}`"))?;
    parse_file_raw(&pkg_dir.join("cackle").join("export.toml"))
}

pub(crate) fn permission_map(config: &RawConfig) -> FxHashMap<PermSel, PackageConfig> {
    let mut packages = FxHashMap::default();
    for (name, crate_config) in &config.packages {
        let mut crate_config = crate_config.clone();
        if let Some(sub_cfg) = crate_config.build.take() {
            packages.insert(
                PermSel {
                    package_name: name.clone(),
                    kind: CrateKind::BuildScript,
                },
                *sub_cfg,
            );
        }
        if let Some(sub_cfg) = crate_config.test.take() {
            packages.insert(
                PermSel {
                    package_name: name.clone(),
                    kind: CrateKind::Test,
                },
                *sub_cfg,
            );
        }
        packages.insert(
            PermSel {
                package_name: name.clone(),
                kind: CrateKind::Primary,
            },
            crate_config,
        );
    }
    packages
}

impl Display for ApiName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Display for ApiPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.prefix)
    }
}

impl From<&'static str> for ApiName {
    fn from(name: &'static str) -> Self {
        ApiName { name: name.into() }
    }
}

impl ApiName {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_owned().into(),
        }
    }
}

impl Config {
    pub(crate) fn get_api_config(&self, api_name: &ApiName) -> Result<&ApiConfig> {
        self.raw
            .apis
            .get(api_name)
            .ok_or_else(|| anyhow!("Missing API config for `{api_name}`"))
    }

    pub(crate) fn unsafe_permitted_for_crate(&self, crate_sel: &CrateSel) -> bool {
        self.permissions
            .get(&crate_sel.non_sandbox_perm_sel())
            .map(|crate_config| crate_config.allow_unsafe)
            .unwrap_or(false)
    }

    /// Returns the configuration for `perm_sel`, inheriting options from the default sandbox
    /// configuration as appropriate.
    pub(crate) fn sandbox_config_for_package(&self, perm_sel: &PermSel) -> SandboxConfig {
        let mut config = self.raw.sandbox.clone();
        let Some(pkg_sandbox_config) = self
            .permissions
            .get(perm_sel)
            .and_then(|c| c.sandbox.as_ref())
        else {
            return config;
        };
        if pkg_sandbox_config.kind != SandboxKind::Inherit {
            config.kind = pkg_sandbox_config.kind;
        }
        config
            .extra_args
            .extend(pkg_sandbox_config.extra_args.iter().cloned());
        config
            .bind_writable
            .extend(pkg_sandbox_config.bind_writable.iter().cloned());
        config
            .make_writable
            .extend(pkg_sandbox_config.make_writable.iter().cloned());
        if let Some(allow_network) = pkg_sandbox_config.allow_network {
            config.allow_network = Some(allow_network);
        }
        config
    }
}

pub(crate) fn flattened_config_path(tmpdir: &Path) -> PathBuf {
    tmpdir.join("flattened_cackle.toml")
}

impl ApiPath {
    pub(crate) fn from_str(prefix: &str) -> Self {
        Self {
            prefix: Arc::from(prefix),
        }
    }
}

impl AsRef<str> for ApiName {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

impl AsRef<str> for ApiPath {
    fn as_ref(&self) -> &str {
        &self.prefix
    }
}

impl From<&str> for PackageName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PermSel {
    // TODO: Some of the callers of this function already have an Arc<str>, but we're currently
    // doing an unnecessary heap allocation.
    pub(crate) fn for_primary(pkg_name: &str) -> Self {
        Self {
            package_name: PackageName(Arc::from(pkg_name)),
            kind: CrateKind::Primary,
        }
    }

    pub(crate) fn for_build_script(pkg_name: &str) -> Self {
        Self {
            package_name: PackageName(Arc::from(pkg_name)),
            kind: CrateKind::BuildScript,
        }
    }

    pub(crate) fn for_test(pkg_name: &str) -> Self {
        Self {
            package_name: PackageName(Arc::from(pkg_name)),
            kind: CrateKind::Test,
        }
    }

    pub(crate) fn is_build_script(&self) -> bool {
        self.kind == CrateKind::BuildScript
    }

    pub(crate) fn is_test(&self) -> bool {
        self.kind == CrateKind::Test
    }
}

impl Display for PackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Display for PermSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.package_name.fmt(f)?;
        if let Some(kind_str) = self.kind.config_selector() {
            '.'.fmt(f)?;
            kind_str.fmt(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::permission_map;
    use super::Config;
    use crate::config_validation::validate;
    use std::sync::Arc;

    pub(crate) fn parse(cackle: &str) -> anyhow::Result<Arc<super::Config>> {
        let cackle_with_header = format!(
            "[common]\nversion = 1\n\
            {cackle}
        "
        );
        let raw = super::parse_raw(&cackle_with_header)?;
        let permissions = permission_map(&raw);
        let config = Config { raw, permissions };
        validate(&config, std::path::Path::new("/dev/null"))?;
        Ok(Arc::new(config))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::parse;
    use crate::config::PermSel;
    use crate::config::SandboxKind;
    use crate::crate_index::CrateIndex;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn empty() {
        let config = parse("").unwrap();
        assert!(config.raw.apis.is_empty());
        assert!(config.permissions.is_empty());
    }

    #[track_caller]
    fn check_unknown_field(context: &str) {
        // Make sure that without the unknown field, it parses OK.
        parse(context).unwrap();
        assert!(parse(&format!("{}\n no_such_field = 1\n", context)).is_err());
    }

    #[test]
    fn unknown_field() {
        check_unknown_field("");
    }

    #[test]
    fn unknown_crate_field() {
        check_unknown_field(
            r#"
            [pkg.foo]
        "#,
        );
    }

    #[test]
    fn unknown_permission_field() {
        check_unknown_field(
            r#"
            [api.foo]
            include = [ "bar" ]
        "#,
        );
    }

    #[test]
    fn unknown_api() {
        let result = parse(
            r#"
            [pkg.foo]
            allow_apis = ["typo"]
        "#,
        );
        assert!(result.is_err());

        let result = parse(
            r#"
            [pkg.foo.build]
            allow_apis = ["typo"]
        "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn crate_build_config() {
        let config = parse(
            r#"
            [pkg.foo.build]
        "#,
        )
        .unwrap();
        assert!(config
            .permissions
            .contains_key(&PermSel::for_build_script("foo")));
    }

    #[test]
    fn sandbox_config_inheritance() {
        let config = parse(
            r#"
                [sandbox]
                kind = "Bubblewrap"
                extra_args = [
                    "--extra1",
                ]

                [pkg.a.build.sandbox]
                extra_args = [
                    "--extra2",
                ]

                [pkg.b.build.sandbox]
                kind = "Disabled"
            "#,
        )
        .unwrap();

        let sandbox_a = config.sandbox_config_for_package(&PermSel::for_build_script("a"));
        assert_eq!(sandbox_a.kind, SandboxKind::Bubblewrap);
        assert_eq!(sandbox_a.extra_args, vec!["--extra1", "--extra2"]);

        let sandbox_b = config.sandbox_config_for_package(&PermSel::for_build_script("b"));
        assert_eq!(sandbox_b.kind, SandboxKind::Disabled);
    }

    #[test]
    fn flattened_config_roundtrips() {
        let crate_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let test_crates_dir = crate_root.join("test_crates");
        let crate_index = CrateIndex::new(&test_crates_dir).unwrap();
        let config = super::parse_file(&test_crates_dir.join("cackle.toml"), &crate_index).unwrap();

        let roundtripped_config =
            Arc::new(super::parse_raw(&config.raw.toml_string().unwrap()).unwrap());
        assert_eq!(config.raw, *roundtripped_config);
    }

    #[test]
    fn duplicate_allow_api() {
        let result = parse(
            r#"
            [api.terminate]
            include = ["std::process::exit"]

            [pkg.foo]
            allow_apis = [
                "terminate",
                "terminate",
            ]
            "#,
        );
        assert!(result.is_err());
        println!("{}", result.as_ref().unwrap_err());
        assert!(result.unwrap_err().to_string().contains("terminate"));
    }
}
