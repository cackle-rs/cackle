//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use crate::checker::Usage;
use crate::checker::UsageLocation;
use crate::config::PermConfig;
use crate::config::PermissionName;
use crate::proxy::rpc::BuildScriptOutput;
use crate::proxy::rpc::UnsafeUsage;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;

#[derive(Default, Debug, PartialEq, Clone)]
pub(crate) struct ProblemList {
    problems: Vec<Problem>,
}

#[must_use]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Problem {
    Message(String),
    Error(ErrorDetails),
    MissingConfiguration(PathBuf),
    UsesBuildScript(String),
    DisallowedUnsafe(UnsafeUsage),
    IsProcMacro(String),
    DisallowedApiUsage(DisallowedApiUsage),
    MultipleSymbolsInSection(MultipleSymbolsInSection),
    BuildScriptFailed(BuildScriptFailed),
    DisallowedBuildInstruction(DisallowedBuildInstruction),
    UnusedPackageConfig(String),
    UnusedAllowApi(UnusedAllowApi),
    SelectSandbox,
    ImportStdApi(PermissionName),
    AvailableApi(AvailableApi),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ErrorDetails {
    pub(crate) short: String,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BuildScriptFailed {
    pub(crate) output: BuildScriptOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisallowedApiUsage {
    pub(crate) pkg_name: String,
    pub(crate) usages: BTreeMap<PermissionName, Vec<Usage>>,
    pub(crate) reachable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UnusedAllowApi {
    pub(crate) pkg_name: String,
    pub(crate) permissions: Vec<PermissionName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DisallowedBuildInstruction {
    pub(crate) pkg_name: String,
    pub(crate) instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AvailableApi {
    pub(crate) pkg_name: String,
    pub(crate) api: PermissionName,
    pub(crate) config: PermConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MultipleSymbolsInSection {
    pub(crate) section_name: SectionName,
    pub(crate) symbols: Vec<Symbol>,
    pub(crate) defined_in: PathBuf,
}

impl ProblemList {
    pub(crate) fn push<T: Into<Problem>>(&mut self, problem: T) {
        self.problems.push(problem.into());
    }

    pub(crate) fn merge(&mut self, mut other: ProblemList) {
        self.problems.append(&mut other.problems);
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Problem> {
        self.problems.get(index)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.problems.len()
    }

    pub(crate) fn replace(&mut self, index: usize, replacement: ProblemList) -> Problem {
        self.problems
            .splice(index..index + 1, replacement.problems.into_iter())
            .next()
            .unwrap()
    }

    pub(crate) fn should_send_retry_to_subprocess(&self) -> bool {
        self.problems
            .iter()
            .all(Problem::should_send_retry_to_subprocess)
    }

    /// Combines all disallowed API usages for a crate.
    #[must_use]
    pub(crate) fn grouped_by_type_and_crate(self) -> ProblemList {
        self.grouped_by(|usage| usage.pkg_name.clone())
    }

    /// Combines all disallowed API usages for a crate and API.
    #[must_use]
    pub(crate) fn grouped_by_type_crate_and_api(self) -> ProblemList {
        self.grouped_by(|usage| match usage.usages.first_key_value() {
            Some((key, _)) => format!("{}-{key}", usage.pkg_name),
            None => usage.pkg_name.clone(),
        })
    }

    /// Combines disallowed API usages by whatever the supplied `group_fn` returns.
    #[must_use]
    fn grouped_by(mut self, group_fn: impl Fn(&DisallowedApiUsage) -> String) -> ProblemList {
        let mut merged = ProblemList::default();
        let mut disallowed_by_crate_name: HashMap<String, usize> = HashMap::new();
        for problem in self.problems.drain(..) {
            match problem {
                Problem::DisallowedApiUsage(usage) => {
                    match disallowed_by_crate_name.entry(group_fn(&usage)) {
                        Entry::Occupied(entry) => {
                            let Problem::DisallowedApiUsage(existing) = &mut merged.problems[*entry.get()] else {
                                panic!("Problems::condense internal error");
                            };
                            for (k, mut v) in usage.usages {
                                existing.usages.entry(k).or_default().append(&mut v);
                            }
                        }
                        Entry::Vacant(entry) => {
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

#[derive(Debug, Clone, Copy)]
pub(crate) enum Severity {
    Warning,
    Error,
}

impl Problem {
    pub(crate) fn new<T: Into<String>>(text: T) -> Self {
        Self::Message(text.into())
    }

    pub(crate) fn severity(&self) -> Severity {
        match self {
            Problem::UnusedAllowApi(..)
            | Problem::UnusedPackageConfig(..)
            | Problem::AvailableApi(..) => Severity::Warning,
            _ => Severity::Error,
        }
    }

    pub(crate) fn details(&self) -> String {
        format!("{self:#}")
    }

    /// Returns whether a retry on this problem needs to be sent to a subprocess.
    fn should_send_retry_to_subprocess(&self) -> bool {
        matches!(
            self,
            &Problem::BuildScriptFailed(..) | &Problem::DisallowedUnsafe(..)
        )
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
            Problem::DisallowedUnsafe(usage) => write!(
                f,
                "Crate {} uses unsafe at {}:{} and doesn't have `allow_unsafe = true`",
                usage.crate_name,
                usage.error_info.file_name.display(),
                usage.error_info.start_line
            )?,
            Problem::UsesBuildScript(pkg_name) => {
                write!(
                    f,
                    "Package {} has a build script, but config file doesn't have [pkg.{}.build]",
                    pkg_name, pkg_name
                )?;
            }
            Problem::IsProcMacro(pkg_name) => write!(
                f,
                "Package `{pkg_name}` is a proc macro but doesn't set allow_proc_macro"
            )?,
            Problem::DisallowedApiUsage(info) => info.fmt(f)?,
            Problem::MultipleSymbolsInSection(info) => {
                writeln!(
                    f,
                    "The section `{}` in `{}` defines multiple symbols:",
                    info.section_name,
                    info.defined_in.display()
                )?;
                for sym in &info.symbols {
                    writeln!(f, "  {sym}")?;
                }
            }
            Problem::BuildScriptFailed(info) => info.fmt(f)?,
            Problem::DisallowedBuildInstruction(info) => {
                write!(
                    f,
                    "{}'s build script emitted disallowed instruction `{}`",
                    info.pkg_name, info.instruction
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
                write!(f, "Package `{}` exports API `{}`", info.pkg_name, info.api)?;
            }
            Problem::Error(info) => {
                if f.alternate() {
                    write!(f, "{}", info.detail)?;
                } else {
                    write!(f, "{}", info.short)?;
                }
            }
        }
        Ok(())
    }
}

impl Display for DisallowedApiUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            if self.reachable == Some(false) {
                writeln!(
                    f,
                    "Crate '{}' uses disallowed APIs, but only from apparently dead code:",
                    self.pkg_name
                )?;
            } else {
                writeln!(f, "Crate '{}' uses disallowed APIs:", self.pkg_name)?;
            }
            for (perm_name, usages) in &self.usages {
                writeln!(f, "  {perm_name}:")?;
                display_usages(f, usages)?;
            }
        } else if self.usages.len() == 1 {
            let (perm, _) = self.usages.first_key_value().unwrap();
            write!(f, "Crate `{}` uses API `{perm}`", self.pkg_name)?;
        } else {
            write!(f, "Crate '{}' uses disallowed APIs: ", self.pkg_name)?;
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
            writeln!(
                f,
                "The config for package '{}' allows the following APIs that aren't used:",
                self.pkg_name
            )?;
            for api in &self.permissions {
                writeln!(f, "    {api}")?;
            }
        } else {
            write!(
                f,
                "Config for `{}` allows APIs that it doesn't use",
                self.pkg_name
            )?;
        }
        Ok(())
    }
}

impl Display for BuildScriptFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Build script for package `{}` failed",
            self.output.package_name
        )?;
        if f.alternate() {
            write!(
                f,
                "\n{}{}",
                String::from_utf8_lossy(&self.output.stderr),
                String::from_utf8_lossy(&self.output.stdout)
            )?;
            if let Ok(Some(sandbox)) = crate::sandbox::from_config(&self.output.sandbox_config) {
                writeln!(
                    f,
                    "Sandbox config:\n{}",
                    sandbox.display_to_run(&self.output.build_script)
                )?;
            }
        }
        Ok(())
    }
}

fn display_usages(f: &mut std::fmt::Formatter, usages: &Vec<Usage>) -> Result<(), std::fmt::Error> {
    let mut by_location: BTreeMap<&UsageLocation, Vec<&Usage>> = BTreeMap::new();
    for u in usages {
        by_location.entry(&u.location).or_default().push(u);
    }
    let mut by_from: BTreeMap<&crate::checker::Referee, Vec<&Symbol>> = BTreeMap::new();
    for (location, usages_for_location) in by_location {
        match location {
            UsageLocation::Source(location) => {
                writeln!(f, "    {}", location.filename.display())?;
            }
            UsageLocation::Unknown(location) => {
                write!(
                    f,
                    "[Unknown source location in `{}`]",
                    to_relative_path(&location.object_path).display()
                )?;
            }
        }
        by_from.clear();
        for usage in usages_for_location {
            by_from.entry(&usage.from).or_default().push(&usage.to);
        }
        for (from, symbols) in &by_from {
            writeln!(f, "      {from}")?;
            for sym in symbols {
                writeln!(f, "        -> {sym}")?;
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

impl std::hash::Hash for DisallowedApiUsage {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.pkg_name.hash(state);
        // Out of laziness, we only hash the permission names, not the usage information.
        for perm in self.usages.keys() {
            perm.hash(state);
        }
        self.reachable.hash(state);
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

#[cfg(test)]
mod tests {
    use super::Problem;
    use super::ProblemList;
    use crate::checker::SourceLocation;
    use crate::checker::Usage;
    use crate::config::PermissionName;
    use crate::symbol::Symbol;
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    #[test]
    fn test_condense() {
        let mut problems = ProblemList::default();
        problems.push(create_problem(
            "foo2",
            &[("net", &[create_usage("bbb", "net_stuff")])],
        ));
        problems.push(create_problem(
            "foo1",
            &[("net", &[create_usage("aaa", "net_stuff")])],
        ));
        problems.push(create_problem(
            "foo1",
            &[("net", &[create_usage("bbb", "net_stuff")])],
        ));
        problems.push(create_problem(
            "foo1",
            &[("fs", &[create_usage("aaa", "fs_stuff")])],
        ));

        problems = problems.grouped_by_type_and_crate();

        let mut package_names = Vec::new();
        for p in &problems.problems {
            if let Problem::DisallowedApiUsage(u) = p {
                package_names.push(u.pkg_name.as_str());
            }
        }
        package_names.sort();
        assert_eq!(package_names, vec!["foo1", "foo2"]);
    }

    fn create_problem(package: &str, permissions_and_usage: &[(&str, &[Usage])]) -> Problem {
        let mut usages = BTreeMap::new();
        for (perm_name, usage) in permissions_and_usage {
            usages.insert(
                PermissionName {
                    name: Cow::Owned(perm_name.to_string()),
                },
                usage.to_vec(),
            );
        }
        Problem::DisallowedApiUsage(super::DisallowedApiUsage {
            pkg_name: package.to_owned(),
            usages,
            reachable: None,
        })
    }

    fn create_usage(from: &str, to: &str) -> Usage {
        Usage {
            location: crate::checker::UsageLocation::Source(SourceLocation {
                filename: "lib.rs".into(),
            }),
            from: crate::checker::Referee::Symbol(Symbol::new(from)),
            to: Symbol::new(to),
        }
    }
}
