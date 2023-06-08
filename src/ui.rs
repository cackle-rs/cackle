//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::problem::Problems;
use anyhow::Result;
pub(crate) use basic_term::BasicTermUi;
pub(crate) use null_ui::NullUi;

mod basic_term;
mod null_ui;

#[derive(Debug, Clone, Copy)]
pub(crate) enum FixOutcome {
    Retry,
    GiveUp,
}

pub(crate) trait Ui {
    /// Prompt the user to fix the supplied problems.
    fn maybe_fix_problems(&mut self, problems: &Problems) -> Result<FixOutcome>;

    fn create_initial_config(&mut self) -> Result<()>;
}
