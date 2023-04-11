//! Some problem - either an error or a permissions problem or similar. We generally collect
//! multiple problems and report them all, although in the case of errors, we usually stop.

use crate::proxy::rpc::CanContinueResponse;
use anyhow::Error;
use anyhow::Result;
use std::fmt::Display;

#[derive(Default, Debug)]
pub(crate) struct Problems {
    problems: Vec<Problem>,
}

#[must_use]
#[derive(Debug)]
pub(crate) enum Problem {
    Message(String),
    Error(Error),
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

impl IntoIterator for Problems {
    type Item = Problem;

    type IntoIter = std::vec::IntoIter<Problem>;

    fn into_iter(self) -> Self::IntoIter {
        self.problems.into_iter()
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
