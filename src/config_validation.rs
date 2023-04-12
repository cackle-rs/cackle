use crate::config::Config;
use crate::config::PermissionName;
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
    UnsupportedVersion(u32),
}

pub(crate) fn validate(config: &Config, config_path: &Path) -> Result<(), InvalidConfig> {
    let mut problems = Vec::new();
    if config.version != 1 {
        problems.push(Problem::UnsupportedVersion(config.version));
    }
    let permission_names: HashSet<_> = config.apis.keys().collect();
    for crate_config in config.packages.values() {
        for permission_name in &crate_config.allow_apis {
            if !permission_names.contains(permission_name) {
                problems.push(Problem::UnknownPermission(permission_name.clone()));
            }
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
                Problem::UnsupportedVersion(version) => {
                    write!(f, "  Unsupported version '{version}'")?
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for InvalidConfig {}
