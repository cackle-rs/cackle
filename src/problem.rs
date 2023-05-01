//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use crate::checker::Usage;
use crate::config::PermissionName;
use crate::proxy::rpc::CanContinueResponse;
use anyhow::Error;
use anyhow::Result;
use std::collections::HashMap;
use std::fmt::Display;

#[derive(Default, Debug, PartialEq)]
pub(crate) struct Problems {
    problems: Vec<Problem>,
}

#[must_use]
#[derive(Debug)]
pub(crate) enum Problem {
    Message(String),
    Error(Error),
    UsesBuildScript(String),
    IsProcMacro(String),
    DisallowedApiUsage(DisallowedApiUsage),
}

#[derive(Debug)]
pub(crate) struct DisallowedApiUsage {
    pub(crate) pkg_name: String,
    pub(crate) usages: HashMap<PermissionName, Vec<Usage>>,
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

    pub(crate) fn from_error<E: Into<Error>>(error: E) -> Self {
        Self::Error(error.into())
    }
}

impl From<String> for Problem {
    fn from(value: String) -> Self {
        Problem::Message(value)
    }
}

impl From<Error> for Problem {
    fn from(value: Error) -> Self {
        Problem::Error(value)
    }
}

impl Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Message(message) => write!(f, "{message}"),
            Problem::Error(error) => write!(f, "{error:?}"),
            Problem::UsesBuildScript(pkg_name) => write!(f, "Package {pkg_name} has a build script, but config file doesn't have [pkg.{pkg_name}.build]"),
            Problem::IsProcMacro(pkg_name) =>  write!(f,
                "Package `{pkg_name}` is a proc macro but doesn't set allow_proc_macro"
            ),
            Problem::DisallowedApiUsage(info) => {
                write!(f, "Crate '{}' uses disallowed APIs:\n", info.pkg_name)?;
                for (perm_name, usages) in &info.usages {
                    write!(f, "  {perm_name}:")?;
                    for usage in usages {
                        writeln!(f, "    {usage}")?;
                    }
                }
                Ok(())
            },
        }
    }
}

impl From<Problem> for Problems {
    fn from(value: Problem) -> Self {
        Self {
            problems: vec![value],
        }
    }
}

impl From<Result<Problems>> for Problems {
    fn from(value: Result<Problems>) -> Self {
        match value {
            Ok(problems) => problems,
            Err(error) => Problem::from_error(error).into(),
        }
    }
}

impl PartialEq for Problem {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Message(l0), Self::Message(r0)) => l0 == r0,
            (Self::Error(l0), Self::Error(r0)) => l0.to_string() == r0.to_string(),
            _ => false,
        }
    }
}
