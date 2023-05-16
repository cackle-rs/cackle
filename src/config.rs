use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) version: u32,

    #[serde(default, rename = "api")]
    pub(crate) apis: HashMap<PermissionName, PermConfig>,

    #[serde(default, rename = "pkg")]
    pub(crate) packages: HashMap<String, PackageConfig>,

    #[serde(default)]
    pub(crate) sandbox: SandboxConfig,

    #[serde(default)]
    pub(crate) ignore_unused: bool,

    #[serde(default)]
    pub(crate) explicit_build_scripts: bool,
}

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxConfig {
    #[serde(default)]
    pub(crate) kind: SandboxKind,

    #[serde(default)]
    pub(crate) allow_read: Vec<String>,

    #[serde(default)]
    pub(crate) extra_args: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
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

#[derive(Deserialize, Debug, Default, Copy, Clone, PartialEq, Eq)]
pub(crate) enum SandboxKind {
    #[default]
    Inherit,
    Disabled,
    Bubblewrap,
}

#[derive(Deserialize, Debug, Clone)]
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

    #[serde()]
    pub(crate) sandbox: Option<SandboxConfig>,
}

pub(crate) fn parse_file(cackle_path: &Path) -> Result<Config> {
    let cackle: String = std::fs::read_to_string(cackle_path)
        .with_context(|| format!("Failed to open {}", cackle_path.display()))?;

    parse(&cackle, cackle_path)
        .with_context(|| format!("Failed to parse {}", cackle_path.display()))
}

pub(crate) fn parse(cackle: &str, cackle_path: &Path) -> Result<Config> {
    let mut config = toml::from_str(cackle)?;
    flatten(&mut config);
    crate::config_validation::validate(&config, cackle_path)?;
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

    pub(crate) fn sandbox_config_for_build_script(&self, package_name: &str) -> SandboxConfig {
        self.sandbox_config_for_package(&format!("{package_name}.build"))
    }

    /// Returns the configuration for `package_name`, inheriting options from the default sandbox
    /// configuration as appropriate.
    pub(crate) fn sandbox_config_for_package(&self, package_name: &str) -> SandboxConfig {
        let mut config = self.sandbox.clone();
        let Some(pkg_sandbox_config) = self.packages.get(package_name).and_then(|c| c.sandbox.as_ref()) else {
            return config;
        };
        if pkg_sandbox_config.kind != SandboxKind::Inherit {
            config.kind = pkg_sandbox_config.kind;
        }
        config
            .extra_args
            .extend(pkg_sandbox_config.extra_args.iter().cloned());
        config
            .allow_read
            .extend(pkg_sandbox_config.allow_read.iter().cloned());
        config
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
    use crate::config::SandboxKind;

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
        assert!(config.packages.contains_key("foo.build"));
    }

    #[test]
    fn sandbox_config_inheritance() {
        let config = parse(
            r#"
                [sandbox]
                kind = "Bubblewrap"
                allow_read = [
                    "/foo",
                    "/bar",
                ]
                extra_args = [
                    "--extra1",
                ]

                [pkg.a.build.sandbox]
                allow_read = [
                    "/baz",
                ]
                extra_args = [
                    "--extra2",
                ]

                [pkg.b.build.sandbox]
                kind = "Disabled"
            "#,
        )
        .unwrap();

        let sandbox_a = config.sandbox_config_for_package("a.build");
        assert_eq!(sandbox_a.kind, SandboxKind::Bubblewrap);
        assert_eq!(sandbox_a.allow_read, vec!["/foo", "/bar", "/baz"]);
        assert_eq!(sandbox_a.extra_args, vec!["--extra1", "--extra2"]);

        let sandbox_b = config.sandbox_config_for_package("b.build");
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
}
