//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use crate::Args;
use anyhow::Result;
use clap::ValueEnum;
use log::info;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
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

trait UserInterface: Send {
    fn run(
        &mut self,
        problem_store: ProblemStoreRef,
        event_receiver: Receiver<AppEvent>,
    ) -> Result<()>;
}

pub(crate) fn start_ui(
    args: &Arc<Args>,
    config_path: &Path,
    problem_store: ProblemStoreRef,
    event_receiver: Receiver<AppEvent>,
) -> Result<JoinHandle<Result<()>>> {
    let mut ui: Box<dyn UserInterface> = match args.ui_kind() {
        Kind::None => {
            info!("Starting null UI");
            Box::new(null_ui::NullUi::new(args))
        }
        Kind::Basic => {
            info!("Starting basic terminal UI");
            Box::new(basic_term::BasicTermUi::new(config_path.to_owned()))
        }
        Kind::Full => {
            info!("Starting full terminal UI");
            Box::new(full_term::FullTermUi::new(config_path.to_owned())?)
        }
    };
    Ok(std::thread::Builder::new()
        .name("UI".to_owned())
        .spawn(move || ui.run(problem_store, event_receiver))?)
}
