use crate::config::PermissionName;
use crate::problem::Problem;
use crate::problem::Problems;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use crate::Args;
use anyhow::bail;
use anyhow::Result;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fmt::Write;
use std::path::Path;
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct Checker {
    permission_names: Vec<PermissionName>,
    permission_name_to_id: HashMap<PermissionName, PermId>,
    inclusions: HashMap<String, Vec<PermId>>,
    exclusions: HashMap<String, Vec<PermId>>,
    pub(crate) crate_infos: Vec<CrateInfo>,
    crate_name_to_index: HashMap<String, CrateId>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct PermId(usize);

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrateId(pub(crate) usize);

pub(crate) const UNKNOWN_CRATE_ID: CrateId = CrateId(0);

#[derive(Default, Debug)]
pub(crate) struct CrateInfo {
    pub(crate) name: Option<String>,

    /// Whether the config file mentions this crate.
    has_config: bool,

    /// Whether a crate with this name was found in the tree. Used to issue a
    /// warning or error if the config refers to a crate that isn't in the
    /// dependency tree.
    used: bool,

    /// Permissions that are allowed for this crate according to cackle.toml.
    allowed_perms: HashSet<PermId>,

    /// Permissions that are allowed for this crate according to cackle.toml,
    /// but haven't yet been found to be used by the crate.
    unused_allowed_perms: HashSet<PermId>,

    /// Permissions that are not permitted for use by this crate but where found to be used (keys)
    /// and the locations of those usages.
    pub(crate) disallowed_usage: HashMap<PermId, Vec<Usage>>,

    /// Whether this crate is a proc macro according to cargo metadata.
    is_proc_macro: bool,

    /// Whether this crate is allowed to be a proc macro according to our config.
    allow_proc_macro: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Usage {
    pub(crate) location: UsageLocation,
    pub(crate) from: Referee,
    pub(crate) to: Symbol,
}

#[derive(Debug, Clone)]
pub(crate) enum Referee {
    Symbol(Symbol),
    Section(SectionName),
}

#[derive(Debug, Clone)]
pub(crate) enum UsageLocation {
    Source(SourceLocation),
    Unknown(UnknownLocation),
}

#[derive(Debug, Clone)]
pub(crate) struct UnknownLocation {
    pub(crate) object_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceLocation {
    pub(crate) filename: PathBuf,
}

#[derive(Default, PartialEq, Eq)]
pub(crate) struct UnusedConfig {
    unknown_crates: Vec<String>,
    unused_allow_apis: HashMap<String, Vec<PermissionName>>,
}

impl Checker {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        let mut checker = Checker::default();

        // Allocate UNKNOWN_CRATE_ID as the first crate.
        checker.crate_infos.push(CrateInfo::default());

        for (perm_name, api) in &config.apis {
            let id = checker.perm_id(perm_name);
            for prefix in &api.include {
                checker
                    .inclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .push(id);
            }
            for prefix in &api.exclude {
                checker
                    .exclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .push(id);
            }
        }
        for (crate_name, crate_config) in &config.packages {
            let crate_id = checker.crate_id_from_name(crate_name);
            let crate_info = &mut checker.crate_infos[crate_id.0];
            crate_info.has_config = true;
            crate_info.allow_proc_macro = crate_config.allow_proc_macro;
            for perm in &crate_config.allow_apis {
                let perm_id = checker.perm_id(perm);
                // Find `crate_info` again. Need to do this here because the `perm_id` above needs
                // to borrow `checker`.
                let crate_info = &mut checker.crate_infos[crate_id.0];
                crate_info.allowed_perms.insert(perm_id);
                crate_info.unused_allowed_perms.insert(perm_id);
            }
        }
        checker
    }

    pub(crate) fn problems(&self, args: &Args) -> Problems {
        let mut problems = Problems::default();
        for crate_info in &self.crate_infos {
            if crate_info.is_proc_macro && !crate_info.allow_proc_macro {
                if let Some(crate_name) = &crate_info.name {
                    problems.push(Problem::new(format!(
                        "Crate `{crate_name}` is a proc macro but doesn't set allow_proc_macro"
                    )));
                }
            }
            if crate_info.disallowed_usage.is_empty() {
                continue;
            }
            let mut problem = if let Some(crate_name) = &crate_info.name {
                format!("Crate '{crate_name}' uses disallowed APIs:\n")
            } else {
                "APIs were used by code where we couldn't identify the crate responsible:\n"
                    .to_owned()
            };
            for (perm_id, usages) in &crate_info.disallowed_usage {
                let perm = self.permission_name(perm_id);
                writeln!(&mut problem, "  {perm}:").unwrap();
                let cap = if args.usage_report_cap < 0 {
                    usages.len()
                } else {
                    args.usage_report_cap as usize
                };
                for usage in usages.iter().take(cap) {
                    writeln!(&mut problem, "    {usage}").unwrap();
                }
            }
            problems.push(Problem::new(problem));
        }
        problems
    }

    pub(crate) fn verify_build_script_permitted(&mut self, package_name: &str) -> Result<()> {
        let pkg_id = self.crate_id_from_name(&format!("{package_name}.build"));
        let crate_info = &mut self.crate_infos[pkg_id.0];
        if !crate_info.has_config {
            bail!("Package {package_name} has a build script, but config file doesn't have [pkg.{package_name}.build]");
        }
        crate_info.used = true;
        Ok(())
    }

    fn perm_id(&mut self, permission: &PermissionName) -> PermId {
        *self
            .permission_name_to_id
            .entry(permission.clone())
            .or_insert_with(|| {
                let perm_id = PermId(self.permission_names.len());
                self.permission_names.push(permission.clone());
                perm_id
            })
    }

    pub(crate) fn permission_name(&self, perm_id: &PermId) -> &PermissionName {
        &self.permission_names[perm_id.0]
    }

    pub(crate) fn crate_id_from_name(&mut self, crate_name: &str) -> CrateId {
        if let Some(id) = self.crate_name_to_index.get(crate_name) {
            return *id;
        }
        let crate_id = CrateId(self.crate_infos.len());
        self.crate_name_to_index
            .insert(crate_name.to_owned(), crate_id);
        self.crate_infos.push(CrateInfo {
            name: Some(crate_name.to_owned()),
            ..CrateInfo::default()
        });
        crate_id
    }

    pub(crate) fn report_crate_used(&mut self, crate_id: CrateId) {
        self.crate_infos[crate_id.0].used = true;
    }

    pub(crate) fn report_proc_macro(&mut self, crate_id: CrateId) {
        self.crate_infos[crate_id.0].is_proc_macro = true;
    }

    /// Report that the specified crate used the path constructed by joining
    /// `name_parts` with "::".
    pub(crate) fn path_used(
        &mut self,
        crate_id: CrateId,
        name_parts: &[String],
        mut compute_usage_fn: impl FnMut() -> Usage,
    ) {
        // TODO: If compute_usage_fn is not expensive, then just pass it in instead of using a
        // closure.
        for perm_id in self.apis_for_path(name_parts) {
            self.permission_id_used(crate_id, perm_id, &mut compute_usage_fn);
        }
    }

    fn apis_for_path(&mut self, name_parts: &[String]) -> HashSet<PermId> {
        let mut matched = HashSet::new();
        let mut name = String::new();
        for name_part in name_parts {
            if !name.is_empty() {
                name.push_str("::");
            }
            name.push_str(name_part);
            for perm_id in self
                .inclusions
                .get(&name)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
            {
                matched.insert(*perm_id);
            }
            for perm_id in self
                .exclusions
                .get(&name)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
            {
                matched.remove(perm_id);
            }
        }
        matched
    }

    fn permission_id_used(
        &mut self,
        crate_id: CrateId,
        perm_id: PermId,
        mut compute_usage_fn: impl FnMut() -> Usage,
    ) {
        let crate_info = &mut self.crate_infos[crate_id.0];
        crate_info.unused_allowed_perms.remove(&perm_id);
        if !crate_info.allowed_perms.contains(&perm_id) {
            crate_info
                .disallowed_usage
                .entry(perm_id)
                .or_default()
                .push((compute_usage_fn)());
        }
    }

    pub(crate) fn check_unused(&self) -> Result<(), UnusedConfig> {
        let mut unused_config = UnusedConfig::default();
        for crate_info in &self.crate_infos {
            let Some(crate_name) = crate_info.name.as_ref() else { continue };
            if !crate_info.used && crate_info.has_config {
                unused_config.unknown_crates.push(crate_name.clone());
            }
            if !crate_info.unused_allowed_perms.is_empty() {
                unused_config.unused_allow_apis.insert(
                    crate_name.clone(),
                    crate_info
                        .unused_allowed_perms
                        .iter()
                        .map(|perm_id| self.permission_names[perm_id.0].clone())
                        .collect(),
                );
            }
        }

        if unused_config == UnusedConfig::default() {
            Ok(())
        } else {
            Err(unused_config)
        }
    }
}

/// Returns `input_path` relative to the current directory, or if that fails, falls back to
/// `input_path`. Only works if `input_path` is absolute and is a subdirectory of the current
/// directory - i.e. it won't use "..".
fn to_relative_path(input_path: &Path) -> &std::path::Path {
    std::env::current_dir()
        .ok()
        .and_then(|current_dir| input_path.strip_prefix(current_dir).ok())
        .unwrap_or(input_path)
}

impl Display for UnusedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.unknown_crates.is_empty() {
            writeln!(
                f,
                "Warning: Config supplied for packages not in dependency tree:"
            )?;
            for crate_name in &self.unknown_crates {
                writeln!(f, "    {crate_name}")?;
            }
        }
        for (pkg_name, used_apis) in &self.unused_allow_apis {
            writeln!(
                f,
                "Warning: The config for package '{pkg_name}' allows the following APIs that aren't used:"
            )?;
            for api in used_apis {
                writeln!(f, "    {api}")?;
            }
        }
        Ok(())
    }
}

impl Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> {} ", self.from, self.to)?;
        match &self.location {
            UsageLocation::Source(location) => {
                write!(f, "[{}]", location.filename.display())?;
            }
            UsageLocation::Unknown(location) => {
                write!(
                    f,
                    "[Unknown source location in `{}`]",
                    to_relative_path(&location.object_path).display()
                )?;
            }
        }
        Ok(())
    }
}

impl Display for Referee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Referee::Symbol(sym) => {
                write!(f, "{sym}")
            }
            Referee::Section(name) => {
                write!(f, "{name}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::parse;

    #[track_caller]
    fn assert_perms(config: &str, path: &[&str], expected: &[&str]) {
        let mut checker = Checker::from_config(&parse(config).unwrap());

        let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        let apis = checker.apis_for_path(&path);
        let mut api_names: Vec<_> = apis
            .iter()
            .map(|perm_id| checker.permission_name(perm_id).to_string())
            .collect();
        api_names.sort();
        assert_eq!(api_names, expected);
    }

    #[test]
    fn test_apis_for_path() {
        let config = r#"
                [api.fs]
                include = [
                    "std::env",
                ]
                exclude = [
                    "std::env::var",
                ]
                
                [api.env]
                include = ["std::env"]

                [api.env2]
                include = ["std::env"]
                "#;
        assert_perms(config, &["std", "env", "var"], &["env", "env2"]);
        assert_perms(config, &["std", "env", "exe"], &["env", "env2", "fs"]);
    }
}
