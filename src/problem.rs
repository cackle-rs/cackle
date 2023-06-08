//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use crate::checker::Usage;
use crate::config::PermissionName;
use crate::proxy::rpc::BuildScriptOutput;
use crate::proxy::rpc::CanContinueResponse;
use crate::proxy::rpc::UnsafeUsage;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;

#[derive(Default, Debug, PartialEq, Clone)]
pub(crate) struct Problems {
    problems: Vec<Problem>,
}

#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    Message(String),
    UsesBuildScript(String),
    DisallowedUnsafe(UnsafeUsage),
    IsProcMacro(String),
    DisallowedApiUsage(DisallowedApiUsage),
    MultipleSymbolsInSection(MultipleSymbolsInSection),
    BuildScriptFailed(BuildScriptFailed),
    DisallowedBuildInstruction(DisallowedBuildInstruction),
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

impl Problems {
    pub(crate) fn push<T: Into<Problem>>(&mut self, problem: T) {
        self.problems.push(problem.into());
    }

    pub(crate) fn merge(&mut self, mut other: Problems) {
        self.problems.append(&mut other.problems);
    }

    pub(crate) fn can_continue(&self) -> CanContinueResponse {
        if self.problems.is_empty() {
            CanContinueResponse::Proceed
        } else {
            CanContinueResponse::Deny
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.problems.len()
    }

    pub(crate) fn remove(&mut self, index: usize) {
        let _ = self.problems.remove(index);
    }

    pub(crate) fn should_send_retry_to_subprocess(&self) -> bool {
        self.problems
            .iter()
            .all(Problem::should_send_retry_to_subprocess)
    }

    /// Combines all disallowed API usages for a crate.
    #[must_use]
    pub(crate) fn grouped_by_type_and_crate(self) -> Problems {
        self.grouped_by(|usage| usage.pkg_name.clone())
    }

    /// Combines all disallowed API usages for a crate.
    #[must_use]
    pub(crate) fn grouped_by_type_crate_and_api(self) -> Problems {
        self.grouped_by(|usage| match usage.usages.first_key_value() {
            Some((key, _)) => format!("{}-{key}", usage.pkg_name),
            None => usage.pkg_name.clone(),
        })
    }

    /// Combines disallowed API usages by whatever the supplied `group_fn` returns.
    #[must_use]
    fn grouped_by(mut self, group_fn: impl Fn(&DisallowedApiUsage) -> String) -> Problems {
        let mut merged = Problems::default();
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

impl std::ops::Index<usize> for Problems {
    type Output = Problem;

    fn index(&self, index: usize) -> &Self::Output {
        &self.problems[index]
    }
}

impl<'a> IntoIterator for &'a Problems {
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
                    for usage in usages {
                        writeln!(f, "    {usage}")?;
                    }
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
        }
        Ok(())
    }
}

impl From<Problem> for Problems {
    fn from(value: Problem) -> Self {
        Self {
            problems: vec![value],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Problem;
    use super::Problems;
    use crate::checker::SourceLocation;
    use crate::checker::Usage;
    use crate::config::PermissionName;
    use crate::symbol::Symbol;
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    #[test]
    fn test_condense() {
        let mut problems = Problems::default();
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
