// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::built_in_perms;
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
    RedefinedBuiltin(PermissionName),
}

pub(crate) fn validate(config: &Config, config_path: &Path) -> Result<(), InvalidConfig> {
    let mut problems = Vec::new();
    let mut permission_names: HashSet<_> = config.perms.keys().collect();
    for built_in in built_in_perms::ALL {
        if !permission_names.insert(built_in) {
            problems.push(Problem::RedefinedBuiltin(built_in.clone()));
        }
    }
    for crate_config in config.crates.values() {
        for permission_name in &crate_config.allow {
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
                Problem::RedefinedBuiltin(x) => {
                    write!(f, "  Redefined built-in permission '{}'", x.name)?
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for InvalidConfig {}
