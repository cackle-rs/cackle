//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use crate::checker::Usage;
use crate::checker::UsageLocation;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    Message(String),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildScriptFailed {
    pub(crate) output: BuildScriptOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisallowedApiUsage {
    pub(crate) pkg_name: String,
    pub(crate) usages: BTreeMap<PermissionName, Vec<Usage>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnusedAllowApi {
    pub(crate) pkg_name: String,
    pub(crate) permissions: Vec<PermissionName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisallowedBuildInstruction {
    pub(crate) pkg_name: String,
    pub(crate) instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub(crate) fn remove(&mut self, index: usize) -> Problem {
        self.problems.remove(index)
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

impl Problem {
    pub(crate) fn new<T: Into<String>>(text: T) -> Self {
        Self::Message(text.into())
    }

    pub(crate) fn short_description(&self) -> String {
        match self {
            Problem::DisallowedApiUsage(info) => {
                if info.usages.len() == 1 {
                    if let Some((perm, _)) = info.usages.first_key_value() {
                        return format!("Crate `{}` uses API `{perm}`", info.pkg_name);
                    }
                }
            }
            Problem::BuildScriptFailed(info) => {
                return format!(
                    "Build script for package `{}` failed",
                    info.output.package_name
                );
            }
            Problem::UnusedAllowApi(info) => {
                return format!(
                    "Config for `{}` allows APIs that it doesn't use",
                    info.pkg_name
                );
            }
            _ => (),
        }
        self.to_string()
    }

    pub(crate) fn details(&self) -> String {
        self.to_string()
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
                usage.crate_name, usage.error_info.file_name.display(), usage.error_info.start_line)?,
            Problem::UsesBuildScript(pkg_name) => write!(f, "Package {pkg_name} has a build script, but config file doesn't have [pkg.{pkg_name}.build]")?,
            Problem::IsProcMacro(pkg_name) =>  write!(f,
                "Package `{pkg_name}` is a proc macro but doesn't set allow_proc_macro"
            )?,
            Problem::DisallowedApiUsage(info) => {
                writeln!(f, "Crate '{}' uses disallowed APIs:", info.pkg_name)?;
                for (perm_name, usages) in &info.usages {
                    writeln!(f, "  {perm_name}:")?;
                    display_usages(f, usages)?;
                }
            },
            Problem::MultipleSymbolsInSection(info) => {
                writeln!(f, "The section `{}` in `{}` defines multiple symbols:",
                    info.section_name, info.defined_in.display())?;
                for sym in &info.symbols {
                    writeln!(f, "  {sym}")?;
                }
            },
            Problem::BuildScriptFailed(info) => {
                writeln!(f, "Build script for package `{}` failed\n{}{}",
                    info.output.package_name,
                    String::from_utf8_lossy(&info.output.stderr),
                    String::from_utf8_lossy(&info.output.stdout))?;
                if let Ok(Some(sandbox)) = crate::sandbox::from_config(&info.output.sandbox_config) {
                    writeln!(f, "Sandbox config:\n{}", sandbox.display_to_run(&info.output.build_script))?;
                }
            }
            Problem::DisallowedBuildInstruction(info) => {
                writeln!(f, "{}'s build script emitted disallowed instruction `{}`",
                    info.pkg_name, info.instruction)?;
            }
            Problem::UnusedPackageConfig(pkg_name) => writeln!(f, "Config supplied for package `{pkg_name}` not in dependency tree")?,
            Problem::UnusedAllowApi(info) => {
                writeln!(
                    f,
                    "The config for package '{}' allows the following APIs that aren't used:",
                    info.pkg_name
                )?;
                for api in &info.permissions {
                    writeln!(f, "    {api}")?;
                }
            },
            Problem::MissingConfiguration(path) => {
                writeln!(f, "Config file `{}` not found", path.display())?;
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
                writeln!(f, "        {sym}")?;
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
