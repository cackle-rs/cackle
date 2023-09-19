use crate::config::ApiName;
use crate::config::Config;
use crate::config::MAX_VERSION;
use fxhash::FxHashSet;
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
    UnknownPermission(ApiName),
    DuplicateAllowedApi(ApiName),
    UnsupportedVersion(i64),
    InvalidPkgSelector(String),
}

pub(crate) fn validate(config: &Config, config_path: &Path) -> Result<(), InvalidConfig> {
    let mut problems = Vec::new();
    if config.raw.common.version < 1 || config.raw.common.version > MAX_VERSION {
        problems.push(Problem::UnsupportedVersion(config.raw.common.version));
    }
    let permission_names: FxHashSet<_> = config.raw.apis.keys().collect();
    for (perm_sel, crate_config) in &config.permissions_no_inheritance.packages {
        let mut used = FxHashSet::default();
        for permission_name in &crate_config.allow_apis {
            if !permission_names.contains(permission_name) {
                problems.push(Problem::UnknownPermission(permission_name.clone()));
            }
            if !used.insert(permission_name) {
                problems.push(Problem::DuplicateAllowedApi(permission_name.clone()))
            }
        }
        if crate_config.build.is_some() {
            problems.push(Problem::InvalidPkgSelector(format!("{perm_sel}.build")));
        }
        if crate_config.test.is_some() {
            problems.push(Problem::InvalidPkgSelector(format!("{perm_sel}.test")));
        }
        if crate_config.from.is_some() {
            problems.push(Problem::InvalidPkgSelector(format!("{perm_sel}.dep")));
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
                Problem::InvalidPkgSelector(sel) => {
                    write!(f, "  Unsupported package selector `pkg.{sel}`")?
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for InvalidConfig {}
