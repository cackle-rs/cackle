//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use fxhash::FxHashMap;
use fxhash::FxHashSet;

use crate::checker::ApiUsage;
use crate::config::ApiPath;
use crate::config::CrateName;
use crate::config::PermConfig;
use crate::config::PermissionName;
use crate::crate_index::BuildScriptId;
use crate::crate_index::CrateSel;
use crate::crate_index::PackageId;
use crate::location::SourceLocation;
use crate::names::SymbolOrDebugName;
use crate::proxy::rpc::BinExecutionOutput;
use crate::proxy::rpc::UnsafeUsage;
use crate::symbol::Symbol;
use std::collections::btree_map;
use std::collections::hash_map;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default, Debug, PartialEq, Clone)]
pub(crate) struct ProblemList {
    problems: Vec<Problem>,
}

#[must_use]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Problem {
    Message(String),
    MissingConfiguration(PathBuf),
    UsesBuildScript(BuildScriptId),
    DisallowedUnsafe(UnsafeUsage),
    IsProcMacro(PackageId),
    DisallowedApiUsage(ApiUsages),
    BuildScriptFailed(BinExecutionFailed),
    DisallowedBuildInstruction(DisallowedBuildInstruction),
    UnusedPackageConfig(CrateName),
    UnusedAllowApi(UnusedAllowApi),
    SelectSandbox,
    ImportStdApi(PermissionName),
    AvailableApi(AvailableApi),
    PossibleExportedApi(PossibleExportedApi),
    UnusedSandboxConfiguration(CrateName),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ErrorDetails {
    pub(crate) short: String,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BinExecutionFailed {
    pub(crate) crate_sel: CrateSel,
    pub(crate) output: BinExecutionOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiUsages {
    pub(crate) crate_sel: CrateSel,
    pub(crate) usages: BTreeMap<PermissionName, Vec<ApiUsage>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UnusedAllowApi {
    pub(crate) crate_name: CrateName,
    pub(crate) permissions: Vec<PermissionName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DisallowedBuildInstruction {
    pub(crate) build_script_id: BuildScriptId,
    pub(crate) instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AvailableApi {
    pub(crate) pkg_id: PackageId,
    pub(crate) api: PermissionName,
    pub(crate) config: PermConfig,
}

/// The name of a top-level module in a crate that matches the name of a restricted API. For
/// example, if there's an API named "fs" and we find a crate with a module named "fs".
#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub(crate) struct PossibleExportedApi {
    pub(crate) pkg_id: PackageId,
    pub(crate) api: PermissionName,
    pub(crate) symbol: Symbol<'static>,
}
impl PossibleExportedApi {
    pub(crate) fn api_path(&self) -> ApiPath {
        ApiPath {
            prefix: Arc::from(format!("{}::{}", self.pkg_id.name(), self.api).as_str()),
        }
    }
}

impl ProblemList {
    pub(crate) fn push<T: Into<Problem>>(&mut self, problem: T) {
        self.problems.push(problem.into());
    }

    pub(crate) fn merge(&mut self, mut other: ProblemList) {
        self.problems.append(&mut other.problems);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.problems.len()
    }

    pub(crate) fn take(self) -> Vec<Problem> {
        self.problems
    }

    pub(crate) fn should_send_retry_to_subprocess(&self) -> bool {
        self.problems
            .iter()
            .all(Problem::should_send_retry_to_subprocess)
    }

    /// Combines all disallowed API usages for a crate and API.
    #[must_use]
    pub(crate) fn grouped_by_type_crate_and_api(self) -> ProblemList {
        self.grouped_by(|usage| match usage.usages.first_key_value() {
            Some((key, _)) => format!("{}-{key}", usage.crate_sel),
            None => usage.crate_sel.to_string(),
        })
    }

    /// Combines disallowed API usages by whatever the supplied `group_fn` returns.
    #[must_use]
    fn grouped_by(mut self, group_fn: impl Fn(&ApiUsages) -> String) -> ProblemList {
        let mut merged = ProblemList::default();
        let mut disallowed_by_crate_name: FxHashMap<String, usize> = FxHashMap::default();
        for problem in self.problems.drain(..) {
            match problem {
                Problem::DisallowedApiUsage(usage) => {
                    match disallowed_by_crate_name.entry(group_fn(&usage)) {
                        hash_map::Entry::Occupied(entry) => {
                            let Problem::DisallowedApiUsage(existing) =
                                &mut merged.problems[*entry.get()]
                            else {
                                panic!("Problems::condense internal error");
                            };
                            for (k, mut v) in usage.usages {
                                existing.usages.entry(k).or_default().append(&mut v);
                            }
                        }
                        hash_map::Entry::Vacant(entry) => {
                            let index = merged.problems.len();
                            merged.push(Problem::DisallowedApiUsage(usage));
                            entry.insert(index);
                        }
                    }
                }
                other => merged.push(other),
            }
        }
        merged
    }
}

impl std::ops::Index<usize> for ProblemList {
    type Output = Problem;

    fn index(&self, index: usize) -> &Self::Output {
        &self.problems[index]
    }
}

impl<'a> IntoIterator for &'a ProblemList {
    type Item = &'a Problem;

    type IntoIter = std::slice::Iter<'a, Problem>;

    fn into_iter(self) -> Self::IntoIter {
        self.problems.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Warning,
    Error,
}

/// Selects how we want problems grouped together. This only affects disallowed API usages.
#[derive(Clone, Copy)]
pub(crate) enum ApiGroupingKind {
    /// Each API is kept separate.
    KeepApisSeparate,

    /// Uses of different APIs by the one crate will be merged into a single group.
    MergeApisWithinCrate,
}

impl Problem {
    pub(crate) fn new<T: Into<String>>(text: T) -> Self {
        Self::Message(text.into())
    }

    pub(crate) fn severity(&self) -> Severity {
        match self {
            Problem::UnusedAllowApi(..)
            | Problem::UnusedPackageConfig(..)
            | Problem::PossibleExportedApi(..)
            | Problem::AvailableApi(..) => Severity::Warning,
            _ => Severity::Error,
        }
    }

    /// Returns whether a retry on this problem needs to be sent to a subprocess.
    fn should_send_retry_to_subprocess(&self) -> bool {
        matches!(
            self,
            &Problem::BuildScriptFailed(..) | &Problem::DisallowedUnsafe(..)
        )
    }

    /// Returns `self` or a clone of `self` with any bits that aren't relevant for deduplication
    /// removed.
    pub(crate) fn deduplication_key(&self, grouping: ApiGroupingKind) -> Problem {
        match (grouping, self) {
            (ApiGroupingKind::KeepApisSeparate, Problem::DisallowedApiUsage(api_usage)) => {
                Problem::DisallowedApiUsage(ApiUsages {
                    crate_sel: api_usage.crate_sel.clone(),
                    usages: api_usage
                        .usages
                        .keys()
                        .map(|key| (key.clone(), vec![]))
                        .collect(),
                })
            }
            (ApiGroupingKind::MergeApisWithinCrate, Problem::DisallowedApiUsage(api_usage)) => {
                Problem::DisallowedApiUsage(ApiUsages {
                    crate_sel: api_usage.crate_sel.clone(),
                    usages: Default::default(),
                })
            }
            (_, Problem::PossibleExportedApi(info)) => {
                Problem::PossibleExportedApi(PossibleExportedApi {
                    symbol: Symbol::borrowed(&[]),
                    ..info.clone()
                })
            }
            _ => self.clone(),
        }
    }

    /// Merges `other` into `self`. Should only be called with two problems that are not equal, but
    /// which have equal deduplication_keys.
    pub(crate) fn merge(&mut self, other: Problem) {
        if let (Problem::DisallowedApiUsage(a), Problem::DisallowedApiUsage(b)) = (self, other) {
            a.merge(b);
        }
    }

    pub(crate) fn pkg_id(&self) -> Option<&PackageId> {
        match self {
            Problem::Message(_) => None,
            Problem::MissingConfiguration(_) => None,
            Problem::UsesBuildScript(build_script_id) => Some(&build_script_id.pkg_id),
            Problem::DisallowedUnsafe(d) => Some(d.crate_sel.pkg_id()),
            Problem::IsProcMacro(pkg_id) => Some(pkg_id),
            Problem::DisallowedApiUsage(d) => Some(d.crate_sel.pkg_id()),
            Problem::BuildScriptFailed(d) => Some(d.crate_sel.pkg_id()),
            Problem::DisallowedBuildInstruction(d) => Some(&d.build_script_id.pkg_id),
            Problem::UnusedPackageConfig(_) => None,
            Problem::UnusedAllowApi(_) => None,
            Problem::SelectSandbox => None,
            Problem::ImportStdApi(_) => None,
            Problem::AvailableApi(d) => Some(&d.pkg_id),
            Problem::PossibleExportedApi(d) => Some(&d.pkg_id),
            Problem::UnusedSandboxConfiguration(_) => None,
        }
    }
}

impl From<String> for Problem {
    fn from(value: String) -> Self {
        Problem::Message(value)
    }
}

impl Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Message(message) => write!(f, "{message}")?,
            Problem::DisallowedUnsafe(usage) => {
                write!(f, "`{}` uses unsafe", usage.crate_sel)?;
                if f.alternate() {
                    writeln!(f)?;
                    for location in &usage.locations {
                        writeln!(f, "{location}")?;
                    }
                }
            }
            Problem::UsesBuildScript(build_script_id) => {
                write!(
                    f,
                    "`{}` has a build script",
                    CrateSel::Primary(build_script_id.pkg_id.clone()),
                )?;
            }
            Problem::IsProcMacro(pkg_name) => write!(
                f,
                "`{}` is a proc macro",
                CrateSel::Primary(pkg_name.clone())
            )?,
            Problem::DisallowedApiUsage(info) => info.fmt(f)?,
            Problem::BuildScriptFailed(info) => info.fmt(f)?,
            Problem::DisallowedBuildInstruction(info) => {
                write!(
                    f,
                    "{}'s build script emitted disallowed instruction `{}`",
                    CrateSel::Primary(info.build_script_id.pkg_id.clone()),
                    info.instruction
                )?;
            }
            Problem::UnusedPackageConfig(pkg_name) => {
                write!(
                    f,
                    "Config supplied for package `{pkg_name}` not in dependency tree"
                )?;
            }
            Problem::UnusedAllowApi(info) => info.fmt(f)?,
            Problem::MissingConfiguration(path) => {
                write!(f, "Config file `{}` not found", path.display())?;
            }
            Problem::SelectSandbox => write!(f, "Select sandbox kind")?,
            Problem::ImportStdApi(api) => write!(f, "Optionally import std API `{api}`")?,
            Problem::AvailableApi(info) => {
                write!(
                    f,
                    "Package `{}` exports API `{}`",
                    CrateSel::Primary(info.pkg_id.clone()),
                    info.api
                )?;
            }
            Problem::PossibleExportedApi(info) => {
                if f.alternate() {
                    write!(
                        f,
                        "Package `{}` provides symbol `{}`. A top-level module has the same name \
                         as the API `{}`. If this module is public (we can't tell), then consider \
                         adjusting the API includes.",
                        info.pkg_id, info.symbol, info.api
                    )?;
                } else {
                    write!(
                        f,
                        "Package `{}` may provide API `{}`",
                        info.pkg_id, info.api
                    )?;
                }
            }
            Problem::UnusedSandboxConfiguration(crate_name) => {
                write!(
                    f,
                    "Having a sandbox configuration for `{crate_name}` doesn't make sense. \
                     Perhaps you meant to configure `{crate_name}.build.sandbox`"
                )?;
            }
        }
        Ok(())
    }
}

impl Display for ApiUsages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            writeln!(f, "'{}' uses disallowed APIs:", self.crate_sel)?;
            for (perm_name, usages) in &self.usages {
                writeln!(f, "  {perm_name}:")?;
                display_usages(f, usages)?;
            }
        } else if self.usages.len() == 1 {
            let (perm, _) = self.usages.first_key_value().unwrap();
            write!(f, "`{}` uses API `{perm}`", self.crate_sel)?;
        } else {
            write!(f, "'{}' uses disallowed APIs: ", self.crate_sel)?;
            let mut first = true;
            for perm_name in self.usages.keys() {
                if first {
                    first = false;
                } else {
                    write!(f, ", ")?;
                }
                write!(f, "{perm_name}")?;
            }
        }
        Ok(())
    }
}

impl Display for UnusedAllowApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            writeln!(f, "`pkg.{}` allows APIs that aren't used:", self.crate_name)?;
            for api in &self.permissions {
                writeln!(f, "    {api}")?;
            }
        } else {
            write!(f, "`pkg.{}` allows APIs that aren't used", self.crate_name)?;
        }
        Ok(())
    }
}

impl Display for BinExecutionFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.output.crate_sel {
            CrateSel::Primary(pkg_id) => {
                write!(f, "Execution of binary for package `{pkg_id}` failed")?;
            }
            CrateSel::BuildScript(build_script_id) => {
                let pkg_id = &build_script_id.pkg_id;
                write!(f, "Build script for package `{pkg_id}` failed")?;
            }
            CrateSel::Test(pkg_id) => {
                write!(f, "Execution of test for package `{pkg_id}` failed")?;
            }
        }
        if f.alternate() {
            write!(
                f,
                "\n{}{}",
                String::from_utf8_lossy(&self.output.stderr),
                String::from_utf8_lossy(&self.output.stdout)
            )?;
            if let Some(sandbox_display) = self.output.sandbox_config_display.as_ref() {
                writeln!(f, "Sandbox config:\n{sandbox_display}",)?;
            }
        }
        Ok(())
    }
}

fn display_usages(
    f: &mut std::fmt::Formatter,
    usages: &Vec<ApiUsage>,
) -> Result<(), std::fmt::Error> {
    let mut by_source_filename: BTreeMap<&Path, Vec<&ApiUsage>> = BTreeMap::new();
    for u in usages {
        by_source_filename
            .entry(u.source_location.filename())
            .or_default()
            .push(u);
    }
    let mut by_from: BTreeMap<&SymbolOrDebugName, Vec<&ApiUsage>> = BTreeMap::new();
    for (filename, usages_for_location) in by_source_filename {
        writeln!(f, "    {}", filename.display())?;
        by_from.clear();
        for usage in usages_for_location {
            by_from.entry(&usage.from).or_default().push(usage);
        }
        for (from, local_usages) in &by_from {
            writeln!(f, "      {from}")?;
            for u in local_usages {
                write!(
                    f,
                    "        -> {} [{}",
                    u.to_source,
                    u.source_location.line(),
                )?;
                if let Some(column) = u.source_location.column() {
                    write!(f, ":{}", column)?;
                }
                writeln!(f, "]")?;
            }
        }
    }
    Ok(())
}

impl From<Problem> for ProblemList {
    fn from(value: Problem) -> Self {
        Self {
            problems: vec![value],
        }
    }
}

impl std::hash::Hash for ApiUsages {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.crate_sel.hash(state);
        // Out of laziness, we only hash the permission names, not the usage information.
        for perm in self.usages.keys() {
            perm.hash(state);
        }
    }
}

/// An opaque key for ApiUsages that can be used in a HashMap for deduplication. Notably, doesn't
/// include the target or debug data. The idea is to collect several usages that are identical
/// except for the target, then pick the shortest of them to show to the user. For example if we
/// have targets of `std::path::PathBuf` and `core::ptr::drop_in_place<std::path::PathBuf>` then the
/// second is redundant. Even if the longer target didn't contain the symbol of the shorter target,
/// it's probably unnecessary to show them all.
#[derive(Hash, Eq, PartialEq)]
pub(crate) struct ApiUsageGroupKey {
    crate_sel: CrateSel,
    permission: PermissionName,
    from: SymbolOrDebugName,
    source_location: SourceLocation,
}

impl ApiUsages {
    pub(crate) fn deduplication_key(&self) -> ApiUsageGroupKey {
        let (permission, usages) = self.usages.iter().next().unwrap();
        let usage = &usages[0];
        ApiUsageGroupKey {
            crate_sel: self.crate_sel.clone(),
            permission: permission.clone(),
            from: usage.from.clone(),
            source_location: usage.source_location.clone(),
        }
    }

    pub(crate) fn first_usage(&self) -> Option<&ApiUsage> {
        self.usages.values().next().and_then(|u| u.get(0))
    }

    fn merge(&mut self, b: ApiUsages) {
        if self.crate_sel != b.crate_sel {
            return;
        }
        for (perm, usages) in b.usages {
            match self.usages.entry(perm) {
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(usages);
                }
                btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    let seen: FxHashSet<_> = existing.iter().collect();
                    let mut to_add = usages.into_iter().filter(|u| !seen.contains(u)).collect();
                    existing.append(&mut to_add);
                }
            }
        }
    }
}
