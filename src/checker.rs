use crate::config::PermissionName;
use anyhow::Result;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;

#[derive(Default)]
pub(crate) struct Checker {
    permission_names: Vec<PermissionName>,
    permission_name_to_id: HashMap<PermissionName, PermId>,
    inclusions: HashMap<String, PermId>,
    exclusions: HashMap<String, PermId>,
    pub(crate) crate_infos: Vec<CrateInfo>,
    crate_name_to_index: HashMap<String, CrateId>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct PermId(usize);

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrateId(pub(crate) usize);

#[derive(Default, Debug)]
pub(crate) struct CrateInfo {
    pub(crate) name: String,
    /// Whether a crate with this name was found in the tree. Used to issue a
    /// warning or error if the config refers to a crate that isn't in the
    /// dependency tree.
    used: bool,
    /// Permissions that are allowed for this crate according to Cackle.toml.
    allowed_perms: HashSet<PermId>,
    /// Permissions that are allowed for this crate according to Cackle.toml,
    /// but haven't yet been found to be used by the crate.
    unused_allowed_perms: HashSet<PermId>,
    /// Permissions that are not permitted for use by this crate but where found
    /// to be used (keys) and the locations of those usages.
    pub(crate) disallowed_usage: HashMap<PermId, Vec<Usage>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Usage {
    pub(crate) filename: String,
    // Line number (0 based)
    pub(crate) line_number: u32,
}

#[derive(Default, PartialEq, Eq)]
pub(crate) struct UnusedConfig {
    unknown_crates: Vec<String>,
    unused_allow_apis: HashMap<String, Vec<PermissionName>>,
}

impl Checker {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        let mut checker = Checker::default();
        for (perm_name, api) in &config.perms {
            let id = checker.perm_id(perm_name);
            for prefix in &api.include {
                checker.inclusions.insert(prefix.to_owned(), id);
            }
            for prefix in &api.exclude {
                checker.exclusions.insert(prefix.to_owned(), id);
            }
        }
        for (crate_name, crate_config) in &config.crates {
            let crate_id = checker.crate_id_from_name(crate_name);
            for perm in &crate_config.allow {
                let perm_id = checker.perm_id(perm);
                let crate_info = &mut checker.crate_infos[crate_id.0];
                crate_info.allowed_perms.insert(perm_id);
                crate_info.unused_allowed_perms.insert(perm_id);
            }
        }
        checker
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
            name: crate_name.to_owned(),
            ..CrateInfo::default()
        });
        crate_id
    }

    pub(crate) fn report_crate_used(&mut self, crate_id: CrateId) {
        self.crate_infos[crate_id.0].used = true;
    }

    /// Report that the specified crate used the path constructed by joining
    /// `name_parts` with "::".
    pub(crate) fn path_used(
        &mut self,
        crate_id: CrateId,
        name_parts: &[String],
        mut compute_usage_fn: impl FnMut() -> Usage,
    ) {
        let mut matched = HashSet::new();
        let mut name = String::new();
        for name_part in name_parts {
            if !name.is_empty() {
                name.push_str("::");
            }
            name.push_str(name_part);
            if let Some(perm_id) = self.inclusions.get(&name) {
                matched.insert(*perm_id);
            }
            if let Some(perm_id) = self.exclusions.get(&name) {
                matched.remove(perm_id);
            }
        }
        if matched.is_empty() {
            return;
        }
        for perm_id in matched {
            self.permission_id_used(crate_id, perm_id, &mut compute_usage_fn);
        }
    }

    pub(crate) fn permission_used(
        &mut self,
        crate_id: CrateId,
        permission: &PermissionName,
        compute_usage_fn: &mut impl FnMut() -> Usage,
    ) {
        let perm_id = self.perm_id(permission);
        self.permission_id_used(crate_id, perm_id, compute_usage_fn);
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
            if !crate_info.used {
                unused_config.unknown_crates.push(crate_info.name.clone());
            }
            if !crate_info.unused_allowed_perms.is_empty() {
                unused_config.unused_allow_apis.insert(
                    crate_info.name.clone(),
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

impl Display for UnusedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.unknown_crates.is_empty() {
            writeln!(
                f,
                "Warning: Config supplied for crates not in dependency tree:"
            )?;
            for crate_name in &self.unknown_crates {
                writeln!(f, "    {crate_name}")?;
            }
        }
        for (crate_name, used_apis) in &self.unused_allow_apis {
            writeln!(
                f,
                "The config for crate '{crate_name}' allows the following APIs that aren't used:"
            )?;
            for api in used_apis {
                writeln!(f, "    {api}")?;
            }
        }
        Ok(())
    }
}
