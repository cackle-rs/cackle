use crate::config::Config;
use crate::config::PermissionName;
use crate::config::MAX_VERSION;
use std::collections::HashSet;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct InvalidConfig {
    config_path: PathBuf,
    problems: Vec<Problem>,
}

#[derive(Debug)]
enum Problem {
    UnknownPermission(PermissionName),
    DuplicateAllowedApi(PermissionName),
    DisallowedSandboxConfig(String),
    UnsupportedVersion(i64),
}

pub(crate) fn validate(config: &Config, config_path: &Path) -> Result<(), InvalidConfig> {
    let mut problems = Vec::new();
    if config.version < 1 || config.version > MAX_VERSION {
        problems.push(Problem::UnsupportedVersion(config.version));
    }
    let permission_names: HashSet<_> = config.apis.keys().collect();
    for (name, crate_config) in &config.packages {
        let mut used = HashSet::new();
        for permission_name in &crate_config.allow_apis {
            if !permission_names.contains(permission_name) {
                problems.push(Problem::UnknownPermission(permission_name.clone()));
            }
            if !used.insert(permission_name) {
                problems.push(Problem::DuplicateAllowedApi(permission_name.clone()))
            }
        }
        if crate_config.sandbox.is_some() && !name.ends_with(".build") {
            problems.push(Problem::DisallowedSandboxConfig(name.clone()))
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(InvalidConfig {
            config_path: config_path.to_owned(),
            problems,
        })
    }
}

impl Display for InvalidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Invalid config {}", self.config_path.display())?;
        for problem in &self.problems {
            match problem {
                Problem::UnknownPermission(x) => write!(f, "  Unknown permission '{}'", x.name)?,
                Problem::DuplicateAllowedApi(x) => {
                    write!(f, "  API allowed more than once '{}'", x.name)?
                }
                Problem::UnsupportedVersion(version) => {
                    write!(f, "  Unsupported version '{version}'")?
                }
                Problem::DisallowedSandboxConfig(pkg_name) => write!(
                    f,
                    "  Sandbox config for regular package `{pkg_name}` isn't permitted"
                )?,
            }
        }
        Ok(())
    }
}

impl std::error::Error for InvalidConfig {}
