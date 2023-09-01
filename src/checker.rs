use crate::build_script_checker;
use crate::config::ApiName;
use crate::config::Config;
use crate::config::CrateName;
use crate::crate_index::CrateIndex;
use crate::crate_index::CrateKind;
use crate::crate_index::CrateSel;
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
use crate::symbol_graph::backtrace::Backtracer;
use crate::symbol_graph::NameSource;
use crate::symbol_graph::UsageDebugData;
use crate::timing::TimingCollector;
use crate::Args;
use crate::CheckState;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use log::info;
use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

mod api_map;
pub(crate) mod common_prefix;

pub(crate) struct Checker {
    /// For each name, the set of APIs active for that name and all names that have this name as a
    /// prefix.
    apis_by_prefix: api_map::ApiMap,
    pub(crate) crate_infos: FxHashMap<CrateName, CrateInfo>,
    config_path: PathBuf,
    pub(crate) config: Arc<Config>,
    target_dir: PathBuf,
    tmpdir: Arc<TempDir>,
    pub(crate) args: Arc<Args>,
    pub(crate) crate_index: Arc<CrateIndex>,

    /// Mapping from Rust source paths to the crate that contains them. Generally a source path will
    /// map to a single crate, but in rare cases multiple crates within a package could use the same
    /// source path.
    path_to_crate: FxHashMap<PathBuf, Vec<CrateSel>>,

    pub(crate) timings: TimingCollector,

    backtracers: FxHashMap<Arc<Path>, Backtracer>,
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
    pub(crate) source_location: SourceLocation,
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
            path_to_crate: Default::default(),
            timings,
            backtracers: Default::default(),
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
        crate::fs::write_atomic(&flattened_path, &config.flattened_toml()?)?;

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
        for api in config.apis.values() {
            for path in api.include.iter().chain(api.exclude.iter()) {
                self.apis_by_prefix
                    .create_entry(crate::names::split_simple(&path.prefix).parts())
            }
        }
        for (api_name, api) in &config.apis {
            for path in &api.include {
                let name = &crate::names::split_simple(&path.prefix);
                self.apis_by_prefix
                    .mut_tree(name.parts())
                    .update_subtree(&|apis| {
                        apis.insert(api_name.clone());
                    });
            }
        }
        for (api_name, api_config) in &config.apis {
            for path in &api_config.exclude {
                let name = &crate::names::split_simple(&path.prefix);
                self.apis_by_prefix
                    .mut_tree(name.parts())
                    .update_subtree(&|apis| {
                        apis.remove(api_name);
                    });
            }
        }
        for (crate_name, crate_config) in &config.packages {
            let crate_info = self.crate_infos.entry(crate_name.clone()).or_default();
            for api in &crate_config.allow_apis {
                if crate_info.allowed_apis.insert(api.clone()) {
                    crate_info.unused_allowed_apis.insert(api.clone());
                }
            }
        }
        self.config = config;
    }

    fn base_problems(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        for pkg_id in self.crate_index.proc_macros() {
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
            rpc::Request::BinExecutionComplete(output) => self.check_build_script_output(output),
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
        if info.crate_sel.kind == CrateKind::BuildScript {
            problems.merge(self.verify_build_script_permitted(&info.crate_sel.pkg_id));
        }
        problems.merge(self.check_object_paths(
            &info.object_paths_under(&self.target_dir),
            &info.output_file,
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
        exe_path: &Path,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        if check_state
            .graph_outputs
            .as_ref()
            .map(|outputs| outputs.apis != self.config.apis)
            .unwrap_or(false)
        {
            // APIs have changed, invalidate cache.
            check_state.graph_outputs = None;
        }
        if check_state.graph_outputs.is_none() {
            let exe_path = Arc::from(exe_path);
            let (mut graph_outputs, backtracer) =
                crate::symbol_graph::scan_objects(paths, &exe_path, self)?;
            graph_outputs.apis = self.config.apis.clone();
            check_state.graph_outputs = Some(graph_outputs);
            self.backtracers.insert(exe_path.clone(), backtracer);
        }
        let graph_outputs = check_state.graph_outputs.as_ref().unwrap();
        let problems = graph_outputs.problems(self)?;
        Ok(problems)
    }

    fn check_build_script_output(&self, output: &rpc::BinExecutionOutput) -> Result<ProblemList> {
        build_script_checker::check(output, &self.config)
    }

    pub(crate) fn crate_uses_unsafe(&self, usage: &UnsafeUsage) -> ProblemList {
        Problem::DisallowedUnsafe(usage.clone()).into()
    }

    pub(crate) fn verify_build_script_permitted(&mut self, pkg_id: &PackageId) -> ProblemList {
        if !self.config.common.explicit_build_scripts {
            return ProblemList::default();
        }
        if self
            .crate_infos
            .contains_key(&CrateName::for_build_script(pkg_id.name()))
        {
            return ProblemList::default();
        }
        Problem::UsesBuildScript(pkg_id.clone()).into()
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
                    .map(|pkg_id| Cow::Owned(vec![CrateSel::primary(pkg_id.clone())]))
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
        if let Some(crate_info) = self
            .crate_infos
            .get_mut(&CrateName::from(&api_usage.crate_sel))
        {
            if crate_info.allowed_apis.contains(api) {
                crate_info.unused_allowed_apis.remove(api);
                return Ok(());
            }
        }

        // Partition all usages into on-tree and off-tree usages. On-tree are those usages that are
        // referencing a name from one of our dependencies. Off-tree are those that reference names
        // from packages not in our package's dependency tree.
        let mut on_tree = Vec::new();
        let mut off_tree: FxHashMap<&PackageId, Vec<ApiUsage>> = FxHashMap::default();

        let all_deps = self.crate_index.name_prefix_to_pkg_id();
        if let Some(crate_deps) = self
            .crate_index
            .transitive_deps(&api_usage.crate_sel.pkg_id)
        {
            for usage in &api_usage.usages {
                if let Some(first_name_part) = usage.to_name.parts.first() {
                    if !crate_deps.contains(first_name_part) {
                        if let Some(pkg_id) = all_deps.get(first_name_part) {
                            off_tree.entry(pkg_id).or_default().push(usage.clone());
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
            let common_from_prefixes = common_prefix::common_from_prefixes(&usages)?;
            problems.push(Problem::OffTreeApiUsage(OffTreeApiUsage {
                usages,
                referenced_pkg_id: pkg_id.clone(),
                common_from_prefixes,
            }));
        }

        // For any remaining on-tree usages, report a regular disallowed API usage.
        if !on_tree.is_empty() {
            problems.push(Problem::DisallowedApiUsage(api_usage.with_usages(on_tree)));
        }
        Ok(())
    }

    pub(crate) fn check_unused(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        let crate_names_in_index: FxHashSet<_> = self.crate_index.crate_names().collect();
        for (crate_name, crate_info) in &self.crate_infos {
            if !crate_names_in_index.contains(crate_name) {
                problems.push(Problem::UnusedPackageConfig(crate_name.clone()));
            }
            if !crate_info.unused_allowed_apis.is_empty() {
                problems.push(Problem::UnusedAllowApi(UnusedAllowApi {
                    crate_name: crate_name.clone(),
                    apis: crate_info.unused_allowed_apis.iter().cloned().collect(),
                }));
            }
        }
        for (crate_name, config) in &self.config.packages {
            if config.sandbox.is_some() && !crate_name.is_build_script() && !crate_name.is_test() {
                problems.push(Problem::UnusedSandboxConfiguration(crate_name.clone()));
            }
        }
        problems
    }

    fn record_crate_paths(&mut self, info: &rpc::RustcOutput) -> Result<()> {
        for path in &info.source_paths {
            let selectors = &mut self.path_to_crate.entry(path.to_owned()).or_default();
            if !selectors.contains(&info.crate_sel) {
                selectors.push(info.crate_sel.clone());
            }
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
            Arc::new(TempDir::new().unwrap()),
            PathBuf::default(),
            Arc::new(Args::default()),
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

        let crate_sel = CrateSel::primary(crate::crate_index::testing::pkg_id("foo"));
        let apis = checker
            .apis_for_name_iterator(["std", "fs", "read_to_string"].into_iter())
            .clone();
        assert_eq!(apis.len(), 1);
        assert_eq!(apis.iter().next().unwrap(), &ApiName::from("fs"));
        for api in apis {
            let api_usage = ApiUsages {
                crate_sel: crate_sel.clone(),
                api_name: api,
                usages: vec![ApiUsage {
                    bin_location: BinLocation {
                        address: 0,
                        symbol_start: 0,
                    },
                    bin_path: Arc::from(Path::new("bin")),
                    source_location: SourceLocation::new(Path::new("lib.rs"), 1, None),
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
        assert!(checker.check_unused().is_empty());

        // Now reload the config and make that we still don't report any unused configuration.
        checker.update_config(config);
        assert!(checker.check_unused().is_empty());
    }
}
