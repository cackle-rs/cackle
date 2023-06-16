use serde::Deserialize;
use serde::Serialize;
use std::fmt::Display;

pub(crate) const SUCCESS: ExitCode = ExitCode(0);
pub(crate) const FAILURE: ExitCode = ExitCode(-1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Outcome {
    Continue,
    GiveUp,
}

/// Our own representation for an ExitCode. We don't use ExitStatus from the standard library
/// because sometimes we need to construct an ExitCode ourselves.
pub(crate) struct ExitCode(pub(crate) i32);

impl ExitCode {
    pub(crate) fn code(&self) -> i32 {
        self.0
    }

    pub(crate) fn is_ok(&self) -> bool {
        self.0 == 0
    }
}

impl From<std::process::ExitStatus> for ExitCode {
    fn from(status: std::process::ExitStatus) -> Self {
        ExitCode(status.code().unwrap_or(-1))
    }
}

impl Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
