use crate::build_script_checker;
use crate::config::permissions::PermSel;
use crate::config::permissions::PermissionScope;
use crate::config::ApiName;
use crate::config::Config;
use crate::crate_index::CrateIndex;
use crate::crate_index::CrateKind;
use crate::crate_index::PackageId;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::names::Name;
use crate::names::SymbolOrDebugName;
use crate::problem::ApiUsages;
use crate::problem::OffTreeApiUsage;
use crate::problem::PossibleExportedApi;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::problem::UnusedAllowApi;
use crate::proxy::rpc;
use crate::proxy::rpc::UnsafeUsage;
use crate::proxy::subprocess::SubprocessConfig;
use crate::symbol_graph::backtrace::Backtracer;
use crate::symbol_graph::NameSource;
use crate::symbol_graph::UsageDebugData;
use crate::timing::TimingCollector;
use crate::tmpdir::TempDir;
use crate::Args;
use crate::CheckState;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use log::info;
use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

mod api_map;
pub(crate) mod common_prefix;

pub(crate) struct Checker {
    /// For each name, the set of APIs active for that name and all names that have this name as a
    /// prefix.
    apis_by_prefix: api_map::ApiMap,
    pub(crate) crate_infos: FxHashMap<PermSel, CrateInfo>,
    config_path: PathBuf,
    pub(crate) config: Arc<Config>,
    target_dir: PathBuf,
    tmpdir: Arc<TempDir>,
    pub(crate) args: Arc<Args>,
    pub(crate) crate_index: Arc<CrateIndex>,
    pub(crate) sysroot: Arc<Path>,

    /// Mapping from Rust source paths to the packages that contains them. Generally a source path
    /// will map to a single package, but in rare cases multiple packages could reference the same
    /// path outside of their source tree.
    path_to_pkg_ids: FxHashMap<PathBuf, Vec<PackageId>>,

    pub(crate) timings: TimingCollector,

    backtracers: FxHashMap<Arc<Path>, Backtracer>,

    /// Information obtained when the linker was invoked, but for which we haven't yet received a
    /// corresponding notification that rustc has completed. We defer processing of these until
    /// rustc completes because we need information from the .deps file that rustc writes.
    outstanding_linker_invocations: Vec<LinkInfo>,
}

#[derive(Default, Debug)]
pub(crate) struct CrateInfo {
    /// APIs that are allowed for this crate according to cackle.toml.
    allowed_apis: FxHashSet<ApiName>,

    /// APIs that are allowed for this crate according to cackle.toml, but haven't yet been found to
    /// be used by the crate.
    unused_allowed_apis: FxHashSet<ApiName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ApiUsage {
    pub(crate) bin_location: BinLocation,
    pub(crate) bin_path: Arc<Path>,
    pub(crate) permission_scope: PermissionScope,
    pub(crate) source_location: SourceLocation,
    /// The source location of the outer (non-inlined) function or variable.
    pub(crate) outer_location: Option<SourceLocation>,
    pub(crate) from: SymbolOrDebugName,
    pub(crate) to: SymbolOrDebugName,
    pub(crate) to_name: Name,
    pub(crate) to_source: NameSource<'static>,
    pub(crate) debug_data: Option<UsageDebugData>,
}

/// A location within a bin file (executable or shared object).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BinLocation {
    pub(crate) address: u64,
    /// The address of the start of the symbol (or section) containing `address`.
    pub(crate) symbol_start: u64,
}

impl Checker {
    pub(crate) fn new(
        tmpdir: Arc<TempDir>,
        target_dir: PathBuf,
        args: Arc<Args>,
        sysroot: Arc<Path>,
        crate_index: Arc<CrateIndex>,
        config_path: PathBuf,
    ) -> Self {
        let timings = TimingCollector::new(args.print_timing);
        Self {
            apis_by_prefix: Default::default(),
            crate_infos: Default::default(),
            config_path,
            config: Default::default(),
            target_dir,
            tmpdir,
            args,
            crate_index,
            path_to_pkg_ids: Default::default(),
            timings,
            backtracers: Default::default(),
            outstanding_linker_invocations: Default::default(),
            sysroot,
        }
    }

    /// Load (or reload) config. Note in the case of reloading, APIs are only ever additive.
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
        crate::fs::write_atomic(
            &flattened_path,
            &SubprocessConfig::from_full_config(&config).serialise()?,
        )?;

        self.update_config(config);
        info!("Config (re)loaded");
        Ok(())
    }

    pub(crate) fn print_timing(&self) {
        println!("{}", self.timings);
    }

    pub(crate) fn get_backtracer(&self, bin_path: &Path) -> Option<&Backtracer> {
        self.backtracers.get(bin_path)
    }

    fn update_config(&mut self, config: Arc<Config>) {
        self.apis_by_prefix.clear();
        for api in config.raw.apis.values() {
            for path in api.include.iter().chain(api.exclude.iter()) {
                self.apis_by_prefix
                    .create_entry(crate::names::split_simple(&path.prefix).parts())
            }
        }
        for (api_name, api) in &config.raw.apis {
            for path in &api.include {
                let name = &crate::names::split_simple(&path.prefix);
                self.apis_by_prefix
                    .mut_tree(name.parts())
                    .update_subtree(&|apis| {
                        apis.insert(api_name.clone());
                    });
            }
        }
        for (api_name, api_config) in &config.raw.apis {
            for path in &api_config.exclude {
                let name = &crate::names::split_simple(&path.prefix);
                self.apis_by_prefix
                    .mut_tree(name.parts())
                    .update_subtree(&|apis| {
                        apis.remove(api_name);
                    });
            }
        }
        // First apply permissions without inheritance, updating our unused_allow_apis records for
        // each selector.
        for (perm_sel, crate_config) in &config.permissions_no_inheritance.packages {
            let crate_info = self.crate_infos.entry(perm_sel.clone()).or_default();
            for api in &crate_config.allow_apis {
                if crate_info.allowed_apis.insert(api.clone()) {
                    crate_info.unused_allowed_apis.insert(api.clone());
                }
            }
        }
        // Then process with inheritance, but leaving unused_allow_apis alone. We don't want to get
        // warnings that an allow_api was unused when it was inherited and was actually used
        // elsewhere in the inheritance tree.
        for (perm_sel, crate_config) in &config.permissions.packages {
            let crate_info = self.crate_infos.entry(perm_sel.clone()).or_default();
            for api in &crate_config.allow_apis {
                crate_info.allowed_apis.insert(api.clone());
            }
        }
        self.config = config;
    }

    fn base_problems(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        for pkg_id in self.crate_index.proc_macros() {
            if !self
                .config
                .permissions
                .get(&PermSel::for_primary(pkg_id.pkg_name()))
                .is_some_and(|pkg_config| pkg_config.allow_proc_macro)
            {
                problems.push(Problem::IsProcMacro(pkg_id.clone()));
            }
        }
        problems
    }

    pub(crate) fn handle_request(
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
                self.outstanding_linker_invocations.push(link_info.clone());
                Ok(ProblemList::default())
            }
            rpc::Request::BinExecutionComplete(output) => {
                if output.exit_code != 0 {
                    Ok(
                        Problem::ExecutionFailed(crate::problem::BinExecutionFailed {
                            output: output.clone(),
                            crate_sel: output.crate_sel.clone(),
                        })
                        .into(),
                    )
                } else if output.crate_sel.kind == CrateKind::BuildScript {
                    let report =
                        build_script_checker::BuildScriptReport::build(output, &self.config)?;
                    crate::sandbox::write_env_vars(
                        self.tmpdir.path(),
                        &output.crate_sel,
                        &report.env_vars,
                    )?;
                    Ok(report.problems)
                } else {
                    Ok(ProblemList::default())
                }
            }
            rpc::Request::RustcComplete(info) => {
                self.record_crate_paths(info)?;
                if let Some(link_info) = self.get_link_info(info) {
                    let problems = self.check_linker_invocation(&link_info, check_state)?;
                    if !problems.is_empty() {
                        // Since we found some problems, add our LinkInfo back so that if we fix the
                        // problems via the UI we can recheck once we have fixes.
                        self.outstanding_linker_invocations.push(link_info);
                    }
                    return Ok(problems);
                }

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
        if info.crate_sel.kind == CrateKind::BuildScript {
            problems.merge(self.verify_build_script_permitted(&info.crate_sel.pkg_id));
        }
        problems.merge(self.check_object_paths(
            &info.object_paths_under(&self.target_dir),
            info,
            check_state,
        )?);
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
        link_info: &LinkInfo,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        if check_state
            .graph_outputs
            .as_ref()
            .is_some_and(|outputs| outputs.apis != self.config.raw.apis)
        {
            // APIs have changed, invalidate cache.
            check_state.graph_outputs = None;
        }
        if check_state.graph_outputs.is_none() {
            let (mut graph_outputs, backtracer) =
                crate::symbol_graph::scan_objects(paths, link_info, self)?;
            graph_outputs.apis = self.config.raw.apis.clone();
            check_state.graph_outputs = Some(graph_outputs);
            self.backtracers
                .insert(link_info.output_file.clone(), backtracer);
        }
        let graph_outputs = check_state.graph_outputs.as_ref().unwrap();
        let problems = graph_outputs.problems(self)?;
        Ok(problems)
    }

    pub(crate) fn crate_uses_unsafe(&self, usage: &UnsafeUsage) -> ProblemList {
        Problem::DisallowedUnsafe(usage.clone()).into()
    }

    pub(crate) fn verify_build_script_permitted(&mut self, pkg_id: &PackageId) -> ProblemList {
        if !self.config.raw.common.explicit_build_scripts {
            return ProblemList::default();
        }
        if self
            .crate_infos
            .contains_key(&PermSel::for_build_script(pkg_id.name_str()))
        {
            return ProblemList::default();
        }
        Problem::UsesBuildScript(pkg_id.clone()).into()
    }

    pub(crate) fn pkg_ids_from_source_path(
        &self,
        source_path: &Path,
    ) -> Result<Cow<Vec<PackageId>>> {
        self.opt_pkg_ids_from_source_path(source_path)
            .ok_or_else(|| anyhow!("Couldn't find crate name for {}", source_path.display(),))
    }

    pub(crate) fn opt_pkg_ids_from_source_path(
        &self,
        source_path: &Path,
    ) -> Option<Cow<Vec<PackageId>>> {
        self.path_to_pkg_ids
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
                    .map(|pkg_id| Cow::Owned(vec![pkg_id.clone()]))
            })
    }

    /// Returns all APIs that are matched by `name`. e.g. The name `["std", "fs", "write"]` might
    /// return the APIs `{"net"}`.
    pub(crate) fn apis_for_name_iterator<'a>(
        &self,
        key_it: impl Iterator<Item = &'a str>,
    ) -> &FxHashSet<ApiName> {
        self.apis_by_prefix.get(key_it)
    }

    /// Reports an API usage. If it's not permitted, then a problem will be added to `problems`.
    pub(crate) fn api_used(
        &mut self,
        api_usage: &ApiUsages,
        problems: &mut ProblemList,
    ) -> Result<()> {
        let api = &api_usage.api_name;
        let perm_sel = api_usage.perm_sel();
        if let Some(crate_info) = self.crate_infos.get_mut(&perm_sel) {
            if crate_info.allowed_apis.contains(api) {
                crate_info.unused_allowed_apis.remove(api);
                self.mark_parent_allow_apis_used(api, &perm_sel);
                return Ok(());
            }
        }

        // Partition all usages into on-tree and off-tree usages. On-tree are those usages that are
        // referencing a name from one of our dependencies. Off-tree are those that reference names
        // from packages not in our package's dependency tree.
        let mut on_tree = Vec::new();
        let mut off_tree: FxHashMap<&PackageId, Vec<ApiUsage>> = FxHashMap::default();

        let all_deps = self.crate_index.name_prefix_to_pkg_id();
        if let Some(crate_deps) = self.crate_index.transitive_deps(&api_usage.pkg_id) {
            for usage in &api_usage.usages {
                if let Some(first_name_part) = usage.to_name.parts.first() {
                    if !crate_deps.contains(first_name_part) {
                        if let Some(pkg_id) = all_deps.get(first_name_part) {
                            // If we detect an off-tree usage where the outer function/variable is
                            // defined by crate that also defined the restricted API that's being
                            // accessed, then we ignore it completely.
                            //
                            // This can happen if for example a macro defines a variable that is
                            // then referenced by an inlined function. The macro and the inlined
                            // function can both be from leaf crates, while the code calling the
                            // macro is from a higher level crate that provides a restricted API.
                            // The end effect is that it looks like the inlined function is
                            // referencing the restricted API.
                            if !self.is_to_name_from_outer_location(usage)? {
                                off_tree.entry(pkg_id).or_default().push(usage.clone());
                            }
                            continue;
                        }
                    }
                }
                on_tree.push(usage.clone());
            }
        }

        // Report off-tree problems for each off-tree package that we appear to reference.
        for (pkg_id, off_tree_usages) in off_tree {
            let usages = api_usage.with_usages(off_tree_usages);
            problems.push(Problem::OffTreeApiUsage(OffTreeApiUsage {
                usages,
                referenced_pkg_id: pkg_id.clone(),
            }));
        }

        // For any remaining on-tree usages, report a regular disallowed API usage.
        if !on_tree.is_empty() {
            problems.push(Problem::DisallowedApiUsage(api_usage.with_usages(on_tree)));
        }
        Ok(())
    }

    /// Returns whether the to-name of `usage` starts with a crate name that matches the package
    /// that defined the outer location of the usage.
    fn is_to_name_from_outer_location(&self, usage: &ApiUsage) -> Result<bool> {
        if let Some(outer_location) = usage.outer_location.as_ref() {
            for pkg_id in self
                .pkg_ids_from_source_path(outer_location.filename())?
                .as_ref()
            {
                let crate_name = pkg_id.crate_name();
                if usage.to_name.starts_with(crate_name.as_ref()) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn check_unused(&self) -> Result<ProblemList> {
        if !self.outstanding_linker_invocations.is_empty() {
            bail!(
                "Linker invocations with no matching rustc completion: {}",
                self.outstanding_linker_invocations.len()
            );
        }

        let mut problems = ProblemList::default();
        let perm_sels_in_index = &self.crate_index.permission_selectors;
        for (perm_sel, crate_info) in &self.crate_infos {
            if !perm_sels_in_index.contains(perm_sel) {
                problems.push(Problem::UnusedPackageConfig(perm_sel.clone()));
            }
            if !crate_info.unused_allowed_apis.is_empty() {
                problems.push(Problem::UnusedAllowApi(UnusedAllowApi {
                    perm_sel: perm_sel.clone(),
                    apis: crate_info.unused_allowed_apis.iter().cloned().collect(),
                }));
            }
        }
        for (perm_sel, config) in &self.config.permissions_no_inheritance.packages {
            if config.sandbox.kind.is_some()
                && !matches!(
                    perm_sel.scope,
                    PermissionScope::Build | PermissionScope::Test
                )
            {
                problems.push(Problem::UnusedSandboxConfiguration(perm_sel.clone()));
            }
        }
        Ok(problems)
    }

    pub(crate) fn check_for_new_config_version(&self) -> ProblemList {
        let version = self.config.raw.common.version;
        if version < crate::config::MAX_VERSION {
            return Problem::NewConfigVersionAvailable(version + 1).into();
        }
        ProblemList::default()
    }

    fn record_crate_paths(&mut self, info: &rpc::RustcOutput) -> Result<()> {
        for path in &info.source_paths {
            let selectors = &mut self.path_to_pkg_ids.entry(path.to_owned()).or_default();
            if !selectors.contains(&info.crate_sel.pkg_id) {
                selectors.push(info.crate_sel.pkg_id.clone());
            }
        }
        Ok(())
    }

    pub(crate) fn print_path_to_crate_map(&self) {
        for (path, crates) in &self.path_to_pkg_ids {
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
            let perm_sel = PermSel::for_primary(p.pkg_id.name_str());
            if let Some(pkg_config) = self.config.permissions.get(&perm_sel) {
                // If we've imported any APIs, or ignored available APIs from the package, then we
                // don't want to report a possible export.
                if pkg_config.import.is_some() {
                    continue;
                }
            }
            if let Some(api_config) = self.config.raw.apis.get(&p.api) {
                if api_config.no_auto_detect.contains(&perm_sel.package_name)
                    || api_config.include.contains(&p.api_path())
                {
                    continue;
                }
            }
            problems.push(Problem::PossibleExportedApi(p.clone()));
        }
    }

    /// Returns the outstanding LinkInfo for when the linker was invoked corresponding to the
    /// supplied rustc completion event.
    fn get_link_info(&mut self, info: &rpc::RustcOutput) -> Option<LinkInfo> {
        let index = self
            .outstanding_linker_invocations
            .iter()
            .position(|link_info| link_info.crate_sel == info.crate_sel)?;
        Some(self.outstanding_linker_invocations.remove(index))
    }

    fn mark_parent_allow_apis_used(&mut self, api: &ApiName, perm_sel: &PermSel) {
        let Some(parent) = perm_sel.parent() else {
            return;
        };
        if let Some(info) = self.crate_infos.get_mut(&parent) {
            info.unused_allowed_apis.remove(api);
        }
        self.mark_parent_allow_apis_used(api, &parent);
    }
}

// Returns whether `source_path` is from the rust standard library or precompiled crates that are
// bundled with the standard library (e.g. hashbrown).
pub(crate) fn is_in_rust_std(source_path: &Path) -> bool {
    source_path.starts_with("/rustc/") || source_path.starts_with("/cargo/registry")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::testing::parse;
    use crate::symbol::Symbol;
    use std::fmt::Debug;
    use std::fmt::Display;

    // Wraps a type T and makes it implement Debug by deferring to the Display implementation of T.
    struct DebugAsDisplay<T: Display>(T);

    impl<T: Display> Debug for DebugAsDisplay<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    fn checker_for_testing() -> Checker {
        Checker::new(
            Arc::new(TempDir::new(None).unwrap()),
            PathBuf::default(),
            Arc::new(Args::default()),
            Arc::from(Path::new("")),
            Arc::new(CrateIndex::default()),
            PathBuf::default(),
        )
    }

    #[track_caller]
    fn assert_apis(config: &str, path: &[&str], expected: &[&str]) {
        let mut checker = checker_for_testing();
        checker.update_config(parse(config).unwrap());

        let apis = checker.apis_for_name_iterator(path.iter().cloned());
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
        assert_apis(config, &["std", "env", "var"], &["env", "env2"]);
        assert_apis(config, &["std", "env", "exe"], &["env", "env2", "fs"]);
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

        let pkg_id = crate::crate_index::testing::pkg_id("foo");
        let apis = checker
            .apis_for_name_iterator(["std", "fs", "read_to_string"].into_iter())
            .clone();
        assert_eq!(apis.len(), 1);
        assert_eq!(apis.iter().next().unwrap(), &ApiName::from("fs"));
        for api in apis {
            let api_usage = ApiUsages {
                pkg_id: pkg_id.clone(),
                scope: crate::config::permissions::PermissionScope::All,
                api_name: api,
                usages: vec![ApiUsage {
                    bin_location: BinLocation {
                        address: 0,
                        symbol_start: 0,
                    },
                    bin_path: Arc::from(Path::new("bin")),
                    permission_scope: PermissionScope::All,
                    source_location: SourceLocation::new(Path::new("lib.rs"), 1, None),
                    outer_location: None,
                    from: SymbolOrDebugName::Symbol(Symbol::borrowed(&[])),
                    to_name: crate::names::split_simple("foo::bar"),
                    to: SymbolOrDebugName::Symbol(Symbol::borrowed(&[])),
                    to_source: NameSource::Symbol(Symbol::borrowed(b"foo::bar")),
                    debug_data: None,
                }],
            };
            checker.api_used(&api_usage, &mut problems).unwrap();
        }

        assert!(problems.is_empty());
        assert_eq!(checker.check_unused().unwrap(), ProblemList::default());

        // Now reload the config and make that we still don't report any unused configuration.
        checker.update_config(config);
        assert!(checker.check_unused().unwrap().is_empty());
    }
}
