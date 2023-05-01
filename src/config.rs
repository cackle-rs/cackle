use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) version: u32,

    #[serde(default, rename = "api")]
    pub(crate) apis: HashMap<PermissionName, PermConfig>,

    #[serde(default, rename = "pkg")]
    pub(crate) packages: HashMap<String, PackageConfig>,

    #[serde(default)]
    pub(crate) sandbox: SandboxConfig,
}

#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxConfig {
    #[serde(default)]
    pub(crate) kind: SandboxKind,

    #[serde(default)]
    pub(crate) allow_read: Vec<String>,

    #[serde(default)]
    pub(crate) extra_args: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermConfig {
    pub(crate) include: Vec<String>,

    #[serde(default)]
    pub(crate) exclude: Vec<String>,
}

#[derive(Deserialize, Default, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone)]
#[serde(transparent)]
pub(crate) struct PermissionName {
    pub(crate) name: Cow<'static, str>,
}

#[derive(Deserialize, Debug, Default)]
pub(crate) enum SandboxKind {
    Disabled,
    #[default]
    Bubblewrap,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageConfig {
    #[serde(default)]
    allow_unsafe: bool,

    #[serde(default)]
    pub(crate) allow_build_instructions: Vec<String>,

    #[serde(default)]
    pub(crate) allow_apis: Vec<PermissionName>,

    #[serde(default)]
    pub(crate) allow_proc_macro: bool,

    /// Configuration for this crate's build.rs. Only used during parsing, after
    /// which it's flattened out.
    build: Option<Box<PackageConfig>>,
}

pub(crate) static DEFAULT_PACKAGE_CONFIG: PackageConfig = PackageConfig {
    allow_unsafe: false,
    allow_build_instructions: vec![],
    allow_apis: vec![],
    allow_proc_macro: false,
    build: None,
};

pub(crate) fn parse_file(cackle_path: &Path) -> Result<Config> {
    let cackle: String = std::fs::read_to_string(cackle_path)
        .with_context(|| format!("Failed to open {}", cackle_path.display()))?;

    parse(&cackle, cackle_path)
}

pub(crate) fn parse(cackle: &str, cackle_path: &Path) -> Result<Config> {
    let mut config = toml::from_str(cackle)?;
    crate::config_validation::validate(&config, cackle_path)?;
    flatten(&mut config);
    Ok(config)
}

fn flatten(config: &mut Config) {
    let mut crates_by_name = HashMap::new();
    for (name, mut crate_config) in config.packages.drain() {
        if let Some(build_config) = crate_config.build.take() {
            crates_by_name.insert(format!("{name}.build"), *build_config);
        }
        crates_by_name.insert(name, crate_config);
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

impl Config {
    pub(crate) fn unsafe_permitted_for_crate(&self, crate_name: &str) -> bool {
        self.packages
            .get(crate_name)
            .map(|crate_config| crate_config.allow_unsafe)
            .unwrap_or(false)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    pub(crate) fn parse(cackle: &str) -> anyhow::Result<super::Config> {
        let cackle_with_header = format!(
            "version = 1\n\
            {cackle}
        "
        );
        super::parse(&cackle_with_header, std::path::Path::new("/dev/null"))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::parse;

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
            [[pkg]]
            name = "foo"
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
        assert!(config.packages.contains_key("foo.build"));
    }
}
