use crate::build_script_checker;
use crate::config::Config;
use crate::config::PermissionName;
use crate::crate_index::CrateIndex;
use crate::link_info::LinkInfo;
use crate::problem::DisallowedApiUsage;
use crate::problem::MultipleSymbolsInSection;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::problem::UnusedAllowApi;
use crate::proxy::rpc;
use crate::proxy::rpc::UnsafeUsage;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use crate::symbol_graph::SymGraph;
use crate::Args;
use crate::CheckState;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use log::info;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Checker {
    permission_names: Vec<PermissionName>,
    permission_name_to_id: HashMap<PermissionName, PermId>,
    inclusions: HashMap<String, HashSet<PermId>>,
    exclusions: HashMap<String, HashSet<PermId>>,
    pub(crate) crate_infos: Vec<CrateInfo>,
    crate_name_to_index: HashMap<String, CrateId>,
    config_path: PathBuf,
    pub(crate) config: Arc<Config>,
    target_dir: PathBuf,
    args: Arc<Args>,
    pub(crate) crate_index: Arc<CrateIndex>,
    /// Mapping from Rust source paths to the crate that contains them. Generally a source path will
    /// map to a single crate, but in rare cases multiple crates within a package could use the same
    /// source path.
    path_to_crate: HashMap<PathBuf, Vec<String>>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct PermId(usize);

#[derive(Debug, Clone, Copy)]
pub(crate) struct CrateId(pub(crate) usize);

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

    /// Whether this crate is a proc macro according to cargo metadata.
    is_proc_macro: bool,

    /// Whether this crate is allowed to be a proc macro according to our config.
    allow_proc_macro: bool,

    /// Whether to ignore references from dead code in this crate.
    ignore_unreachable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) location: UsageLocation,
    pub(crate) from: Referee,
    pub(crate) to: Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Referee {
    Symbol(Symbol),
    Section(SectionName),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UsageLocation {
    Source(SourceLocation),
    Unknown(UnknownLocation),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UnknownLocation {
    pub(crate) object_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SourceLocation {
    pub(crate) filename: PathBuf,
}

#[derive(Default, PartialEq, Eq)]
pub(crate) struct UnusedConfig {
    unknown_crates: Vec<String>,
    unused_allow_apis: HashMap<String, Vec<PermissionName>>,
}

impl Checker {
    pub(crate) fn new(
        target_dir: PathBuf,
        args: Arc<Args>,
        crate_index: Arc<CrateIndex>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            permission_names: Default::default(),
            permission_name_to_id: Default::default(),
            inclusions: Default::default(),
            exclusions: Default::default(),
            crate_infos: Default::default(),
            crate_name_to_index: Default::default(),
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
            let id = self.perm_id(perm_name);
            for prefix in &api.include {
                self.inclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .insert(id);
            }
            for prefix in &api.exclude {
                self.exclusions
                    .entry(prefix.to_owned())
                    .or_default()
                    .insert(id);
            }
        }
        for (crate_name, crate_config) in &config.packages {
            let crate_id = self.crate_id_from_name(crate_name);
            let crate_info = &mut self.crate_infos[crate_id.0];
            crate_info.has_config = true;
            crate_info.allow_proc_macro = crate_config.allow_proc_macro;
            crate_info.ignore_unreachable = crate_config.ignore_unreachable;
            for perm in &crate_config.allow_apis {
                let perm_id = self.perm_id(perm);
                // Find `crate_info` again. Need to do this here because the `perm_id` above needs
                // to borrow `checker`.
                let crate_info = &mut self.crate_infos[crate_id.0];
                if crate_info.allowed_perms.insert(perm_id) {
                    crate_info.unused_allowed_perms.insert(perm_id);
                }
            }
        }
        self.config = config;
    }

    fn base_problems(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        for crate_info in &self.crate_infos {
            if crate_info.is_proc_macro && !crate_info.allow_proc_macro {
                if let Some(crate_name) = &crate_info.name {
                    problems.push(Problem::IsProcMacro(crate_name.clone()));
                }
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
        problems.merge(
            self.check_object_paths(&info.object_paths_under(&self.target_dir), check_state)?,
        );
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
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        if self.args.debug {
            println!(
                "{}",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if check_state.graph.is_none() {
            let start = std::time::Instant::now();
            let mut graph = SymGraph::default();
            for path in paths {
                graph
                    .process_file(path)
                    .with_context(|| format!("Failed to process `{}`", path.display()))?;
            }
            if self.args.print_timing {
                println!("Graph computation took {}ms", start.elapsed().as_millis());
            }
            check_state.graph = Some(graph);
        }
        let graph = check_state.graph.as_mut().unwrap();
        if self.args.print_all_references {
            println!("{graph}");
        }
        // In order to save some computation, we skip computing reachabilty unless we actually need
        // it. The two cases where we need it are (1) if any package sets ignore_unreachable and (2)
        // if the user interface is active, since in that case we might want to suggest
        // ignore_unreachable as an edit.
        if self.config.needs_reachability() || matches!(self.args.command, crate::Command::Ui(..)) {
            let result = graph.compute_reachability(&self.args);
            if result.is_err() && self.args.verbose_errors {
                println!("Object paths:");
                for p in paths {
                    println!("  {}", p.display());
                }
            }
            result?;
        }
        let start = std::time::Instant::now();
        let problems = graph.problems(self)?;
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

    pub(crate) fn multiple_symbols_in_section(
        &mut self,
        defined_in: &Path,
        symbols: &[Symbol],
        section_name: &SectionName,
        problems: &mut ProblemList,
    ) {
        problems.push(Problem::MultipleSymbolsInSection(
            MultipleSymbolsInSection {
                section_name: section_name.clone(),
                symbols: symbols.to_owned(),
                defined_in: defined_in.to_owned(),
            },
        ));
    }

    pub(crate) fn verify_build_script_permitted(&mut self, package_name: &str) -> ProblemList {
        let pkg_id = self.crate_id_from_name(&format!("{package_name}.build"));
        let crate_info = &mut self.crate_infos[pkg_id.0];
        if !crate_info.has_config && self.config.common.explicit_build_scripts {
            return Problem::UsesBuildScript(package_name.to_owned()).into();
        }
        crate_info.used = true;
        ProblemList::default()
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

    pub(crate) fn crate_names_from_source_path(
        &mut self,
        source_path: &Path,
        ref_path: &Path,
    ) -> Result<Vec<String>> {
        self.path_to_crate
            .get(Path::new(source_path))
            .ok_or_else(|| {
                anyhow!(
                    "Couldn't find crate name for {} referenced from {}",
                    source_path.display(),
                    ref_path.display()
                )
            })
            .cloned()
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

    pub(crate) fn ignore_unreachable(&self, crate_id: CrateId) -> bool {
        self.crate_infos[crate_id.0]
            .ignore_unreachable
            .unwrap_or(self.config.common.ignore_unreachable)
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
        problems: &mut ProblemList,
        reachable: Option<bool>,
        mut compute_usage_fn: impl FnMut() -> Usage,
    ) {
        // TODO: If compute_usage_fn is not expensive, then just pass it in instead of using a
        // closure.
        for perm_id in self.apis_for_path(name_parts) {
            self.permission_id_used(
                crate_id,
                perm_id,
                problems,
                reachable,
                &mut compute_usage_fn,
            );
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
            let empty_hash_set = HashSet::new();
            for perm_id in self.inclusions.get(&name).unwrap_or(&empty_hash_set) {
                matched.insert(*perm_id);
            }
            for perm_id in self.exclusions.get(&name).unwrap_or(&empty_hash_set) {
                matched.remove(perm_id);
            }
        }
        matched
    }

    fn permission_id_used(
        &mut self,
        crate_id: CrateId,
        perm_id: PermId,
        problems: &mut ProblemList,
        reachable: Option<bool>,
        mut compute_usage_fn: impl FnMut() -> Usage,
    ) {
        let crate_info = &mut self.crate_infos[crate_id.0];
        crate_info.unused_allowed_perms.remove(&perm_id);
        if !crate_info.allowed_perms.contains(&perm_id) {
            let Some(pkg_name) = &crate_info.name else {
                    problems.push(Problem::new("APIs were used by code where we couldn't identify the crate responsible"));
                    return;
                };
            let pkg_name = pkg_name.clone();
            let permission_name = self.permission_name(&perm_id).clone();
            let mut usages = BTreeMap::new();
            usages.insert(permission_name, vec![compute_usage_fn()]);
            problems.push(Problem::DisallowedApiUsage(DisallowedApiUsage {
                pkg_name,
                usages,
                reachable,
            }));
        }
    }

    pub(crate) fn check_unused(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        for crate_info in &self.crate_infos {
            let Some(crate_name) = crate_info.name.as_ref() else { continue };
            if !crate_info.used && crate_info.has_config {
                problems.push(Problem::UnusedPackageConfig(crate_name.clone()));
            }
            if !crate_info.unused_allowed_perms.is_empty() {
                problems.push(Problem::UnusedAllowApi(UnusedAllowApi {
                    pkg_name: crate_name.clone(),
                    permissions: crate_info
                        .unused_allowed_perms
                        .iter()
                        .map(|perm_id| self.permission_names[perm_id.0].clone())
                        .collect(),
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
    use std::fmt::Debug;

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
        let mut checker = Checker::default();
        checker.update_config(config.clone());
        let crate_id = checker.crate_id_from_name("foo");
        let mut problems = ProblemList::default();

        checker.report_crate_used(crate_id);
        checker.path_used(
            crate_id,
            &[
                "std".to_owned(),
                "fs".to_owned(),
                "read_to_string".to_owned(),
            ],
            &mut problems,
            None,
            || Usage {
                location: crate::checker::UsageLocation::Source(SourceLocation {
                    filename: "lib.rs".into(),
                }),
                from: crate::checker::Referee::Symbol(Symbol::new(vec![])),
                to: Symbol::new(vec![]),
            },
        );

        assert!(problems.is_empty());
        assert!(checker.check_unused().is_empty());

        // Now reload the config and make that we still don't report any unused configuration.
        checker.update_config(config);
        assert!(checker.check_unused().is_empty());
    }
}
