//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use super::FixOutcome;
use super::Ui;
use crate::problem::Problems;
use anyhow::Result;

pub(crate) struct NullUi;

impl Ui for NullUi {
    fn maybe_fix_problems(&mut self, _problems: &Problems) -> Result<FixOutcome> {
        Ok(FixOutcome::GiveUp)
    }

    fn create_initial_config(&mut self) -> Result<FixOutcome> {
        // We'll error later when we try to read the configuration.
        Ok(FixOutcome::Retry)
    }
}
