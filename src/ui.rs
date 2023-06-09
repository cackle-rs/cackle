//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::problem::Problems;
use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;
use std::path::Path;

mod basic_term;
mod full_term;
mod null_ui;

#[derive(ValueEnum, Debug, Clone, Copy)]
pub(crate) enum Kind {
    None,
    Basic,
    Full,
}

pub(crate) fn create(kind: Kind, config_path: &Path) -> Result<Box<dyn Ui>> {
    Ok(match kind {
        Kind::None => Box::new(null_ui::NullUi),
        Kind::Basic => Box::new(basic_term::BasicTermUi::new(config_path.to_owned())),
        Kind::Full => Box::new(full_term::FullTermUi::new(config_path.to_owned())?),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixOutcome {
    Retry,
    GiveUp,
}

pub(crate) trait Ui {
    /// Prompt the user to fix the supplied problems.
    fn maybe_fix_problems(&mut self, problems: &Problems) -> Result<FixOutcome>;

    fn create_initial_config(&mut self) -> Result<FixOutcome>;

    fn report_error(&mut self, error: &anyhow::Error) -> Result<()> {
        println!("{} {:#}", "ERROR:".red(), error);
        Ok(())
    }

    fn display_message(&mut self, title: &str, message: &str) -> Result<()> {
        println!("{title}\n{message}");
        Ok(())
    }
}
