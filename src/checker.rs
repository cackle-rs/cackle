use crate::build_script_checker;
use crate::config::ApiPath;
use crate::config::Config;
use crate::config::CrateName;
use crate::config::PermissionName;
use crate::crate_index::CrateIndex;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::names::Name;
use crate::problem::ApiUsages;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::problem::UnusedAllowApi;
use crate::proxy::rpc;
use crate::proxy::rpc::UnsafeUsage;
use crate::symbol::Symbol;
use crate::symbol_graph::object_file_path::ObjectFilePath;
use crate::symbol_graph::UsageDebugData;
use crate::Args;
use crate::CheckState;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use log::info;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Checker {
    inclusions: HashMap<ApiPath, HashSet<PermissionName>>,
    exclusions: HashMap<ApiPath, HashSet<PermissionName>>,
    pub(crate) crate_infos: HashMap<CrateName, CrateInfo>,
    config_path: PathBuf,
    pub(crate) config: Arc<Config>,
    target_dir: PathBuf,
    pub(crate) args: Arc<Args>,
    pub(crate) crate_index: Arc<CrateIndex>,
    /// Mapping from Rust source paths to the crate that contains them. Generally a source path will
    /// map to a single crate, but in rare cases multiple crates within a package could use the same
    /// source path.
    path_to_crate: HashMap<PathBuf, Vec<CrateName>>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct PermId(usize);

#[derive(Default, Debug)]
pub(crate) struct CrateInfo {
    /// Whether the config file mentions this crate.
    has_config: bool,

    /// Permissions that are allowed for this crate according to cackle.toml.
    allowed_perms: HashSet<PermissionName>,

    /// Permissions that are allowed for this crate according to cackle.toml,
    /// but haven't yet been found to be used by the crate.
    unused_allowed_perms: HashSet<PermissionName>,

    /// Whether this crate is a proc macro according to cargo metadata.
    is_proc_macro: bool,

    /// Whether this crate is allowed to be a proc macro according to our config.
    allow_proc_macro: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiUsage {
    pub(crate) source_location: SourceLocation,
    pub(crate) from: Symbol<'static>,
    pub(crate) to: Name,
    pub(crate) to_symbol: Symbol<'static>,
    pub(crate) debug_data: Option<UsageDebugData>,
}

impl Checker {
    pub(crate) fn new(
        target_dir: PathBuf,
        args: Arc<Args>,
        crate_index: Arc<CrateIndex>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            inclusions: Default::default(),
            exclusions: Default::default(),
            crate_infos: Default::default(),
            config_path,
            config: Default::default(),
            target_dir,
            args,
            crate_index,
            path_to_crate: Default::default(),
        }
    }

    /// Load (or reload) config. Note in the case of reloading, permissions are only ever additive.
    pub(crate) fn load_config(&mut self) -> Result<()> {
        let config = crate::config::parse_file(&self.config_path, &self.crate_index)?;
        // Every time we reload our configuration, we rewrite the flattened configuration. The
        // flattened configuration is used by subprocesses rather than using the original
        // configuration since using the original would require each subprocess to run `cargo
        // metadata`.
        let flattened_path = crate::config::flattened_config_path(&self.target_dir);
        if let Some(dir) = flattened_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create directory `{}`", dir.display()))?;
        }
        // A subprocess might try to read the flattened config while we're updating it. It doesn't
        // matter if it sees the old or the new flattened config, but we don't want it to see a
        // partially written config, so we write first to a temporary file then rename it.
        crate::fs::write_atomic(&flattened_path, &config.flattened_toml()?)?;

        self.update_config(config);
        info!("Config (re)loaded");
        Ok(())
    }

    fn update_config(&mut self, config: Arc<Config>) {
        for (perm_name, api) in &config.apis {
            for prefix in &api.include {
                self.inclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .insert(perm_name.clone());
            }
            for prefix in &api.exclude {
                self.exclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .insert(perm_name.clone());
            }
        }
        for (crate_name, crate_config) in &config.packages {
            let crate_info = self
                .crate_infos
                .entry(crate_name.as_ref().into())
                .or_default();
            crate_info.has_config = true;
            crate_info.allow_proc_macro = crate_config.allow_proc_macro;
            for perm in &crate_config.allow_apis {
                if crate_info.allowed_perms.insert(perm.clone()) {
                    crate_info.unused_allowed_perms.insert(perm.clone());
                }
            }
        }
        self.config = config;
    }

    fn base_problems(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        for (crate_name, crate_info) in &self.crate_infos {
            if crate_info.is_proc_macro && !crate_info.allow_proc_macro {
                problems.push(Problem::IsProcMacro(crate_name.clone()));
            }
        }
        problems
    }

    pub(crate) fn problems(
        &mut self,
        request: &Option<rpc::Request>,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        let Some(request) = request else {
            return Ok(self.base_problems());
        };
        match request {
            rpc::Request::CrateUsesUnsafe(usage) => Ok(self.crate_uses_unsafe(usage)),
            rpc::Request::LinkerInvoked(link_info) => {
                self.check_linker_invocation(link_info, check_state)
            }
            rpc::Request::BuildScriptComplete(output) => Ok(self.check_build_script_output(output)),
            rpc::Request::RustcComplete(info) => {
                self.record_crate_paths(info);
                Ok(ProblemList::default())
            }
            rpc::Request::RustcStarted(crate_name) => {
                info!("Rustc started compiling {crate_name}");
                Ok(ProblemList::default())
            }
        }
    }

    fn check_linker_invocation(
        &mut self,
        info: &LinkInfo,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        let mut problems = ProblemList::default();
        if info.is_build_script {
            problems.merge(self.verify_build_script_permitted(&info.package_name));
        }
        problems.merge(self.check_object_paths(
            &info.object_paths_under(&self.target_dir),
            &info.output_file,
            check_state,
        )?);
        let problems = problems.grouped_by_type_crate_and_api();
        info!(
            "Checking linker args for {} with {} objects. {} problems",
            info.package_name,
            info.object_paths.len(),
            problems.len(),
        );
        Ok(problems)
    }

    pub(crate) fn check_object_paths(
        &mut self,
        paths: &[PathBuf],
        exe_path: &Path,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        if check_state.graph_outputs.is_none() {
            let start = std::time::Instant::now();
            let graph_outputs = crate::symbol_graph::scan_objects(paths, exe_path, self)?;
            if self.args.print_timing {
                println!("Graph computation took {}ms", start.elapsed().as_millis());
            }
            check_state.graph_outputs = Some(graph_outputs);
        }
        let graph_outputs = check_state.graph_outputs.as_ref().unwrap();
        let start = std::time::Instant::now();
        let problems = graph_outputs.problems(self)?;
        if self.args.print_timing {
            println!("API usage checking took {}ms", start.elapsed().as_millis());
        }
        Ok(problems)
    }

    fn check_build_script_output(&self, output: &rpc::BuildScriptOutput) -> ProblemList {
        build_script_checker::check(output, &self.config)
    }

    pub(crate) fn crate_uses_unsafe(&self, usage: &UnsafeUsage) -> ProblemList {
        Problem::DisallowedUnsafe(usage.clone()).into()
    }

    pub(crate) fn verify_build_script_permitted(&mut self, package_name: &str) -> ProblemList {
        if !self.config.common.explicit_build_scripts {
            return ProblemList::default();
        }
        let crate_name = CrateName::from(format!("{package_name}.build").as_str());
        if let Some(crate_info) = self.crate_infos.get_mut(&crate_name) {
            if crate_info.has_config {
                return ProblemList::default();
            }
        }
        Problem::UsesBuildScript(crate_name).into()
    }

    pub(crate) fn crate_names_from_source_path(
        &self,
        source_path: &Path,
        ref_path: &ObjectFilePath,
    ) -> Result<Vec<CrateName>> {
        self.path_to_crate
            .get(source_path)
            .cloned()
            .or_else(|| {
                // Fall-back to just finding the crate that contains the source path.
                self.crate_index
                    .crate_name_for_path(source_path)
                    .map(|crate_name| vec![crate_name.clone()])
            })
            .ok_or_else(|| {
                anyhow!(
                    "Couldn't find crate name for {} referenced from {ref_path}",
                    source_path.display(),
                )
            })
    }

    pub(crate) fn report_proc_macro(&mut self, crate_name: &CrateName) {
        self.crate_infos
            .entry(crate_name.clone())
            .or_default()
            .is_proc_macro = true;
    }

    /// Returns all permissions that are matched by `name`. e.g. The name `["std", "fs", "write"]`
    /// might return the APIs `{"net"}`
    pub(crate) fn apis_for_name(&self, name: &Name) -> HashSet<PermissionName> {
        let mut matched = HashSet::new();
        let mut prefix = String::new();
        for name_part in &name.parts {
            if !prefix.is_empty() {
                prefix.push_str("::");
            }
            prefix.push_str(name_part);
            let empty_hash_set = HashSet::new();
            let api_path = ApiPath::from_str(&prefix);
            for perm_id in self.inclusions.get(&api_path).unwrap_or(&empty_hash_set) {
                matched.insert(perm_id.clone());
            }
            for perm_id in self.exclusions.get(&api_path).unwrap_or(&empty_hash_set) {
                matched.remove(perm_id);
            }
        }
        matched
    }

    pub(crate) fn permission_used(&mut self, api_usage: &ApiUsages, problems: &mut ProblemList) {
        assert_eq!(api_usage.usages.keys().count(), 1);
        let permission = api_usage.usages.keys().next().unwrap();
        let crate_info = &mut self
            .crate_infos
            .entry(api_usage.crate_name.clone())
            .or_default();
        if crate_info.allowed_perms.contains(permission) {
            crate_info.unused_allowed_perms.remove(permission);
        } else {
            problems.push(Problem::DisallowedApiUsage(api_usage.clone()));
        }
    }

    pub(crate) fn check_unused(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        let crate_names: HashSet<_> = self.crate_index.crate_names().collect();
        for (crate_name, crate_info) in &self.crate_infos {
            if crate_info.has_config && !crate_names.contains(crate_name) {
                problems.push(Problem::UnusedPackageConfig(crate_name.clone()));
            }
            if !crate_info.unused_allowed_perms.is_empty() {
                problems.push(Problem::UnusedAllowApi(UnusedAllowApi {
                    crate_name: crate_name.clone(),
                    permissions: crate_info.unused_allowed_perms.iter().cloned().collect(),
                }));
            }
        }
        problems
    }

    fn record_crate_paths(&mut self, info: &rpc::RustcOutput) {
        for path in &info.source_paths {
            self.path_to_crate
                .entry(path.to_owned())
                .or_default()
                .push(info.crate_name.clone());
        }
    }

    pub(crate) fn print_path_to_crate_map(&self) {
        for (path, crates) in &self.path_to_crate {
            for c in crates {
                println!("{} -> {}", path.display(), c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Debug;
    use std::fmt::Display;

    use super::*;
    use crate::config::testing::parse;

    // Wraps a type T and makes it implement Debug by deferring to the Display implementation of T.
    struct DebugAsDisplay<T: Display>(T);

    impl<T: Display> Debug for DebugAsDisplay<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    #[track_caller]
    fn assert_perms(config: &str, path: &[&str], expected: &[&str]) {
        let mut checker = Checker::default();
        checker.update_config(parse(config).unwrap());

        let parts: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        let apis = checker.apis_for_name(&Name { parts });
        let mut api_names: Vec<_> = apis.iter().map(AsRef::as_ref).collect();
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

    #[test]
    fn reload_config() {
        let config = parse(
            r#"
            [api.fs]
            include = [
                "std::fs",
            ]
            [pkg.foo]
            allow_apis = [
                "fs",
            ]
        "#,
        )
        .unwrap();
        let mut checker = Checker {
            crate_index: crate::crate_index::testing::index_with_crate_names(&["foo"]),
            ..Checker::default()
        };
        checker.update_config(config.clone());
        let mut problems = ProblemList::default();

        let crate_name = CrateName::from("foo");
        let permissions = checker.apis_for_name(&Name {
            parts: vec![
                "std".to_owned(),
                "fs".to_owned(),
                "read_to_string".to_owned(),
            ],
        });
        assert_eq!(permissions.len(), 1);
        assert_eq!(
            permissions.iter().next().unwrap(),
            &PermissionName::from("fs")
        );
        for api in permissions {
            let mut usages = BTreeMap::new();
            usages.insert(
                api,
                vec![ApiUsage {
                    source_location: SourceLocation::new("lib.rs", 1, None),
                    from: Symbol::borrowed(&[]),
                    to: crate::names::split_names("foo:bar").pop().unwrap(),
                    to_symbol: Symbol::borrowed(&[]),
                    debug_data: None,
                }],
            );
            let api_usage = ApiUsages {
                crate_name: crate_name.clone(),
                usages,
            };
            checker.permission_used(&api_usage, &mut problems);
        }

        assert!(problems.is_empty());
        assert!(checker.check_unused().is_empty());

        // Now reload the config and make that we still don't report any unused configuration.
        checker.update_config(config);
        assert!(checker.check_unused().is_empty());
    }
}
