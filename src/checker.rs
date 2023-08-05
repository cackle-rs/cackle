use crate::build_script_checker;
use crate::config::ApiPath;
use crate::config::Config;
use crate::config::CrateName;
use crate::config::PermissionName;
use crate::crate_index::BuildScriptId;
use crate::crate_index::CrateIndex;
use crate::crate_index::CrateSel;
use crate::crate_index::PackageId;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::names::Name;
use crate::problem::ApiUsages;
use crate::problem::PossibleExportedApi;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::problem::UnusedAllowApi;
use crate::proxy::rpc;
use crate::proxy::rpc::UnsafeUsage;
use crate::symbol::Symbol;
use crate::symbol_graph::NameSource;
use crate::symbol_graph::UsageDebugData;
use crate::timing::TimingCollector;
use crate::Args;
use crate::CheckState;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use log::info;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

pub(crate) struct Checker {
    inclusions: HashMap<ApiPath, HashSet<PermissionName>>,
    exclusions: HashMap<ApiPath, HashSet<PermissionName>>,
    proc_macros: HashSet<PackageId>,
    pub(crate) crate_infos: HashMap<CrateName, CrateInfo>,
    config_path: PathBuf,
    pub(crate) config: Arc<Config>,
    target_dir: PathBuf,
    tmpdir: Arc<TempDir>,
    pub(crate) args: Arc<Args>,
    pub(crate) crate_index: Arc<CrateIndex>,

    /// Mapping from Rust source paths to the crate that contains them. Generally a source path will
    /// map to a single crate, but in rare cases multiple crates within a package could use the same
    /// source path.
    path_to_crate: HashMap<PathBuf, Vec<CrateSel>>,

    pub(crate) timings: TimingCollector,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct PermId(usize);

#[derive(Default, Debug)]
pub(crate) struct CrateInfo {
    /// Permissions that are allowed for this crate according to cackle.toml.
    allowed_perms: HashSet<PermissionName>,

    /// Permissions that are allowed for this crate according to cackle.toml,
    /// but haven't yet been found to be used by the crate.
    unused_allowed_perms: HashSet<PermissionName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiUsage {
    pub(crate) source_location: SourceLocation,
    pub(crate) from: Symbol<'static>,
    pub(crate) to: Name,
    pub(crate) to_symbol: Symbol<'static>,
    pub(crate) to_source: NameSource<'static>,
    pub(crate) debug_data: Option<UsageDebugData>,
}

impl Checker {
    pub(crate) fn new(
        tmpdir: Arc<TempDir>,
        target_dir: PathBuf,
        args: Arc<Args>,
        crate_index: Arc<CrateIndex>,
        config_path: PathBuf,
    ) -> Self {
        let timings = TimingCollector::new(args.print_timing);
        Self {
            inclusions: Default::default(),
            exclusions: Default::default(),
            crate_infos: Default::default(),
            config_path,
            config: Default::default(),
            target_dir,
            tmpdir,
            args,
            crate_index,
            path_to_crate: Default::default(),
            proc_macros: Default::default(),
            timings,
        }
    }

    /// Load (or reload) config. Note in the case of reloading, permissions are only ever additive.
    pub(crate) fn load_config(&mut self) -> Result<()> {
        let config = crate::config::parse_file(&self.config_path, &self.crate_index)?;
        // Every time we reload our configuration, we rewrite the flattened configuration. The
        // flattened configuration is used by subprocesses rather than using the original
        // configuration since using the original would require each subprocess to run `cargo
        // metadata`.
        let flattened_path = crate::config::flattened_config_path(self.tmpdir.path());
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

    pub(crate) fn print_timing(&self) {
        println!("{}", self.timings);
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
        for pkg_id in &self.proc_macros {
            if !self
                .config
                .packages
                .get(&pkg_id.into())
                .map(|pkg_config| pkg_config.allow_proc_macro)
                .unwrap_or(false)
            {
                problems.push(Problem::IsProcMacro(pkg_id.clone()));
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
            rpc::Request::BuildScriptComplete(output) => self.check_build_script_output(output),
            rpc::Request::RustcComplete(info) => {
                self.record_crate_paths(info)?;
                Ok(ProblemList::default())
            }
            rpc::Request::RustcStarted(crate_sel) => {
                info!("Rustc started compiling {crate_sel}");
                Ok(ProblemList::default())
            }
        }
    }

    fn check_linker_invocation(
        &mut self,
        info: &LinkInfo,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        let start = std::time::Instant::now();
        let mut problems = ProblemList::default();
        if let CrateSel::BuildScript(build_script_id) = &info.crate_sel {
            problems.merge(self.verify_build_script_permitted(build_script_id));
        }
        problems.merge(self.check_object_paths(
            &info.object_paths_under(&self.target_dir),
            &info.output_file,
            check_state,
        )?);
        let problems = problems.grouped_by_type_crate_and_api();
        self.timings.add_timing(start, "Total object processing");
        info!(
            "Checking linker args for {} with {} objects. {} problems",
            info.crate_sel,
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
            let graph_outputs = crate::symbol_graph::scan_objects(paths, exe_path, self)?;
            check_state.graph_outputs = Some(graph_outputs);
        }
        let graph_outputs = check_state.graph_outputs.as_ref().unwrap();
        let problems = graph_outputs.problems(self)?;
        Ok(problems)
    }

    fn check_build_script_output(&self, output: &rpc::BuildScriptOutput) -> Result<ProblemList> {
        build_script_checker::check(output, &self.config)
    }

    pub(crate) fn crate_uses_unsafe(&self, usage: &UnsafeUsage) -> ProblemList {
        Problem::DisallowedUnsafe(usage.clone()).into()
    }

    pub(crate) fn verify_build_script_permitted(
        &mut self,
        build_script_id: &BuildScriptId,
    ) -> ProblemList {
        if !self.config.common.explicit_build_scripts {
            return ProblemList::default();
        }
        if self
            .crate_infos
            .contains_key(&CrateName::from(build_script_id))
        {
            return ProblemList::default();
        }
        Problem::UsesBuildScript(build_script_id.clone()).into()
    }

    pub(crate) fn crate_names_from_source_path(
        &self,
        source_path: &Path,
    ) -> Result<Cow<Vec<CrateSel>>> {
        self.opt_crate_names_from_source_path(source_path)
            .ok_or_else(|| anyhow!("Couldn't find crate name for {}", source_path.display(),))
    }

    pub(crate) fn opt_crate_names_from_source_path(
        &self,
        source_path: &Path,
    ) -> Option<Cow<Vec<CrateSel>>> {
        self.path_to_crate
            .get(source_path)
            .map(Cow::Borrowed)
            .or_else(|| {
                // If the source path is from the rust standard library, or from one of the
                // precompiled crates that comes with the standard library, then report no crates.
                if is_in_rust_std(source_path) {
                    return Some(Cow::Owned(vec![]));
                }

                // Fall-back to just finding the package that contains the source path.
                self.crate_index
                    .package_id_for_path(source_path)
                    .map(|pkg_id| Cow::Owned(vec![CrateSel::Primary(pkg_id.clone())]))
            })
    }

    pub(crate) fn report_proc_macro(&mut self, pkg_id: &PackageId) {
        self.proc_macros.insert(pkg_id.clone());
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
        if let Some(crate_info) = self
            .crate_infos
            .get_mut(&CrateName::from(&api_usage.crate_sel))
        {
            if crate_info.allowed_perms.contains(permission) {
                crate_info.unused_allowed_perms.remove(permission);
                return;
            }
        }
        problems.push(Problem::DisallowedApiUsage(api_usage.clone()));
    }

    pub(crate) fn check_unused(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        let crate_names_in_index: HashSet<_> = self.crate_index.crate_names().collect();
        for (crate_name, crate_info) in &self.crate_infos {
            if !crate_names_in_index.contains(crate_name) {
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

    fn record_crate_paths(&mut self, info: &rpc::RustcOutput) -> Result<()> {
        for path in &info.source_paths {
            self.path_to_crate
                .entry(path.to_owned())
                .or_default()
                .push(info.crate_sel.clone());
        }
        Ok(())
    }

    pub(crate) fn print_path_to_crate_map(&self) {
        for (path, crates) in &self.path_to_crate {
            for c in crates {
                println!("{c} -> {}", path.display());
            }
        }
    }

    pub(crate) fn possible_exported_api_problems(
        &self,
        possible_exported_apis: &[PossibleExportedApi],
        problems: &mut ProblemList,
    ) {
        for p in possible_exported_apis {
            let crate_name = CrateName::from(&p.pkg_id);
            if let Some(pkg_config) = self.config.packages.get(&crate_name) {
                // If we've imported any APIs, or ignored available APIs from the package, then we
                // don't want to report a possible export.
                if pkg_config.import.is_some() {
                    continue;
                }
            }
            if let Some(api_config) = self.config.apis.get(&p.api) {
                if api_config.no_auto_detect.contains(&crate_name)
                    || api_config.include.contains(&p.api_path())
                {
                    continue;
                }
            }
            problems.push(Problem::PossibleExportedApi(p.clone()));
        }
    }
}

// Returns whether `source_path` is from the rust standard library or precompiled crates that are
// bundled with the standard library (e.g. hashbrown).
pub(crate) fn is_in_rust_std(source_path: &Path) -> bool {
    source_path.starts_with("/rustc/") || source_path.starts_with("/cargo/registry")
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

    fn checker_for_testing() -> Checker {
        Checker::new(
            Arc::new(TempDir::new().unwrap()),
            PathBuf::default(),
            Arc::new(Args::default()),
            Arc::new(CrateIndex::default()),
            PathBuf::default(),
        )
    }

    #[track_caller]
    fn assert_perms(config: &str, path: &[&str], expected: &[&str]) {
        let mut checker = checker_for_testing();
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
            crate_index: crate::crate_index::testing::index_with_package_names(&["foo"]),
            ..checker_for_testing()
        };
        checker.update_config(config.clone());
        let mut problems = ProblemList::default();

        let crate_sel = CrateSel::Primary(crate::crate_index::testing::pkg_id("foo"));
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
                    source_location: SourceLocation::new(Path::new("lib.rs"), 1, None),
                    from: Symbol::borrowed(&[]),
                    to: crate::names::split_names("foo:bar").pop().unwrap(),
                    to_symbol: Symbol::borrowed(&[]),
                    to_source: NameSource::Symbol(Symbol::borrowed(b"foo::bar")),
                    debug_data: None,
                }],
            );
            let api_usage = ApiUsages {
                crate_sel: crate_sel.clone(),
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
