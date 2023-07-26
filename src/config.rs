use crate::crate_index::BuildScriptId;
use crate::crate_index::CrateIndex;
use crate::crate_index::PackageId;
use crate::problem::AvailableApi;
use crate::problem::Problem;
use crate::problem::ProblemList;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) mod built_in;

pub(crate) const MAX_VERSION: i64 = 1;

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) common: CommonConfig,

    #[serde(default, rename = "api")]
    pub(crate) apis: BTreeMap<PermissionName, PermConfig>,

    #[serde(default, rename = "pkg")]
    pub(crate) packages: BTreeMap<CrateName, PackageConfig>,

    #[serde(default)]
    pub(crate) sandbox: SandboxConfig,
}

/// Selects either the primary crate of a package or the build script of a crate. In the latter
/// case, the format of the name is for example "foo.build" where foo is the name of the primary
/// package. This struct is like CrateSel, but without any version information. We use this when
/// we're referring to the cackle configuration, since we don't distinguish between different
/// versions of crates when granting permissions.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub(crate) struct CrateName(pub(crate) Arc<str>);

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
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxConfig {
    #[serde(default)]
    pub(crate) kind: SandboxKind,

    #[serde(default)]
    pub(crate) extra_args: Vec<String>,

    pub(crate) allow_network: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Default, Hash)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermConfig {
    pub(crate) include: Vec<ApiPath>,

    #[serde(default)]
    pub(crate) exclude: Vec<ApiPath>,
}

#[derive(Deserialize, Serialize, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone)]
#[serde(transparent)]
pub(crate) struct PermissionName {
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
    pub(crate) allow_apis: Vec<PermissionName>,

    #[serde(default)]
    pub(crate) allow_proc_macro: bool,

    /// Configuration for this crate's build.rs. Only used during parsing, after
    /// which it's flattened out.
    build: Option<Box<PackageConfig>>,

    #[serde()]
    pub(crate) sandbox: Option<SandboxConfig>,

    #[serde(default)]
    pub(crate) import: Option<Vec<String>>,
}

pub(crate) fn parse_file(cackle_path: &Path, crate_index: &CrateIndex) -> Result<Arc<Config>> {
    let cackle: String = std::fs::read_to_string(cackle_path)
        .with_context(|| format!("Failed to open {}", cackle_path.display()))?;

    let mut config =
        parse(&cackle).with_context(|| format!("Failed to parse {}", cackle_path.display()))?;
    config.load_imports(crate_index)?;
    crate::config_validation::validate(&config, cackle_path)?;
    Ok(Arc::new(config))
}

fn parse(cackle: &str) -> Result<Config> {
    let mut config = toml::from_str(cackle)?;
    merge_built_ins(&mut config)?;
    flatten(&mut config);
    Ok(config)
}

fn merge_built_ins(config: &mut Config) -> Result<()> {
    if config.common.import_std.is_empty() {
        return Ok(());
    }
    let built_ins = built_in::get_built_ins();
    for imp in config.common.import_std.drain(..) {
        let perm = PermissionName::new(imp.as_str());
        let built_in_api = built_ins
            .get(&perm)
            .ok_or_else(|| anyhow!("Unknown API `{imp}` in import_std"))?;
        let api = config.apis.entry(perm).or_insert_with(Default::default);
        api.include.extend(built_in_api.include.iter().cloned());
        api.exclude.extend(built_in_api.exclude.iter().cloned());
    }
    Ok(())
}

impl Config {
    fn load_imports(&mut self, crate_index: &CrateIndex) -> Result<()> {
        for (crate_name, pkg_config) in &mut self.packages {
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
                .newest_package_id_with_name(crate_name)
                .ok_or_else(|| {
                    anyhow!(
                        "Attempted to import APIs from package `{crate_name}` that wasn't found"
                    )
                })?;
            let pkg_exports = exported_config_for_package(pkg_id, crate_index)?;
            for (api_name, api_def) in &pkg_exports.apis {
                if !imports.iter().any(|imp| imp == api_name.name.as_ref()) {
                    // The user didn't request importing this API, so skip it.
                    continue;
                }
                let qualified_api_name = PermissionName {
                    name: format!("{}::{}", crate_name, api_name).into(),
                };
                if self
                    .apis
                    .insert(qualified_api_name.clone(), api_def.clone())
                    .is_some()
                {
                    bail!(
                        "[pkg.{}.api.{}] is defined multiple times",
                        crate_name,
                        api_name
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn flattened_toml(&self) -> Result<String> {
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
                .get(&pkg_id.into())
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

/// Attempts to load "cackle/export.toml" from the specified package.
fn exported_config_for_package(
    pkg_id: &PackageId,
    crate_index: &CrateIndex,
) -> Result<Arc<Config>> {
    let pkg_dir = crate_index
        .pkg_dir(pkg_id)
        .ok_or_else(|| anyhow!("Missing pkg_dir for package `{pkg_id}`"))?;
    parse_file(&pkg_dir.join("cackle").join("export.toml"), crate_index)
}

fn flatten(config: &mut Config) {
    let mut crates_by_name = BTreeMap::new();
    for (name, crate_config) in &config.packages {
        let mut crate_config = crate_config.clone();
        if let Some(build_config) = crate_config.build.take() {
            crates_by_name.insert(format!("{name}.build").as_str().into(), *build_config);
        }
        crates_by_name.insert(name.clone(), crate_config);
    }
    config.packages = crates_by_name;
}

impl Display for PermissionName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl From<&'static str> for PermissionName {
    fn from(name: &'static str) -> Self {
        PermissionName { name: name.into() }
    }
}

impl PermissionName {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_owned().into(),
        }
    }
}

impl Config {
    pub(crate) fn unsafe_permitted_for_crate(&self, crate_name: &CrateName) -> bool {
        self.packages
            .get(crate_name)
            .map(|crate_config| crate_config.allow_unsafe)
            .unwrap_or(false)
    }

    pub(crate) fn sandbox_config_for_build_script(
        &self,
        build_script_id: &BuildScriptId,
    ) -> SandboxConfig {
        self.sandbox_config_for_package(&CrateName::from(build_script_id))
    }

    /// Returns the configuration for `package_name`, inheriting options from the default sandbox
    /// configuration as appropriate.
    pub(crate) fn sandbox_config_for_package(&self, package_name: &CrateName) -> SandboxConfig {
        let mut config = self.sandbox.clone();
        let Some(pkg_sandbox_config) = self
            .packages
            .get(package_name)
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
        if let Some(allow_network) = pkg_sandbox_config.allow_network {
            config.allow_network = Some(allow_network);
        }
        config
    }
}

pub(crate) fn flattened_config_path(target_dir: &Path) -> PathBuf {
    target_dir
        .join(crate::proxy::cargo::PROFILE_NAME)
        .join("flattened_cackle.toml")
}

impl ApiPath {
    pub(crate) fn from_str(prefix: &str) -> Self {
        Self {
            prefix: Arc::from(prefix),
        }
    }
}

impl AsRef<str> for PermissionName {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

impl AsRef<str> for ApiPath {
    fn as_ref(&self) -> &str {
        &self.prefix
    }
}

impl From<&str> for CrateName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl AsRef<str> for CrateName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl CrateName {
    pub(crate) fn for_build_script(crate_name: &str) -> Self {
        Self(Arc::from(format!("{crate_name}.build").as_str()))
    }
}

impl Display for CrateName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use crate::config_validation::validate;
    use std::sync::Arc;

    pub(crate) fn parse(cackle: &str) -> anyhow::Result<Arc<super::Config>> {
        let cackle_with_header = format!(
            "[common]\nversion = 1\n\
            {cackle}
        "
        );
        let config = Arc::new(super::parse(&cackle_with_header)?);
        validate(&config, std::path::Path::new("/dev/null"))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::parse;
    use crate::config::SandboxKind;
    use crate::crate_index::CrateIndex;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn empty() {
        let config = parse("").unwrap();
        assert!(config.apis.is_empty());
        assert!(config.packages.is_empty());
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
        assert!(config.packages.contains_key(&"foo.build".into()));
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

        let sandbox_a = config.sandbox_config_for_package(&"a.build".into());
        assert_eq!(sandbox_a.kind, SandboxKind::Bubblewrap);
        assert_eq!(sandbox_a.extra_args, vec!["--extra1", "--extra2"]);

        let sandbox_b = config.sandbox_config_for_package(&"b.build".into());
        assert_eq!(sandbox_b.kind, SandboxKind::Disabled);
    }

    #[test]
    fn disallowed_sandbox_override() {
        // A sandbox configuration for a regular package isn't allowed, since we don't run regular
        // packages.
        let result = parse(
            r#"
                [pkg.a.sandbox]
                kind = "Disabled"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn flattened_config_roundtrips() {
        let crate_root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let test_crates_dir = crate_root.join("test_crates");
        let crate_index = CrateIndex::new(&test_crates_dir).unwrap();
        let config = super::parse_file(&test_crates_dir.join("cackle.toml"), &crate_index).unwrap();

        let roundtripped_config =
            Arc::new(super::parse(&config.flattened_toml().unwrap()).unwrap());
        assert_eq!(config, roundtripped_config);
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
