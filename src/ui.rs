//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use anyhow::Result;
use clap::ValueEnum;
use colored::Colorize;
use log::info;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;

mod basic_term;
mod full_term;
mod null_ui;

#[derive(ValueEnum, Debug, Clone, Copy)]
pub(crate) enum Kind {
    None,
    Basic,
    Full,
}

pub(crate) fn start_ui(
    kind: Kind,
    config_path: &Path,
    problem_store: ProblemStoreRef,
    event_receiver: Receiver<AppEvent>,
) -> Result<JoinHandle<Result<()>>> {
    Ok(match kind {
        Kind::None => {
            info!("Starting null UI");
            let ui = null_ui::NullUi::new();
            std::thread::spawn(move || ui.run(problem_store, event_receiver))
        }
        Kind::Basic => {
            info!("Starting basic terminal UI");
            let ui = basic_term::BasicTermUi::new(config_path.to_owned());
            std::thread::spawn(move || ui.run(problem_store, event_receiver))
        }
        Kind::Full => {
            info!("Starting full terminal UI");
            let ui = full_term::FullTermUi::new(config_path.to_owned())?;
            std::thread::spawn(move || ui.run(problem_store, event_receiver))
        }
    })
}

// TODO: Do we need both this can CanContinueResponse
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixOutcome {
    Continue,
    GiveUp,
}

pub(crate) trait Ui {
    fn start_problem_solving(&mut self, problem_store: ProblemStoreRef);

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
