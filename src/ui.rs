//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::checker::Checker;
use crate::crate_index::CrateIndex;
use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use crate::Args;
use crate::Command;
use anyhow::Result;
use clap::Parser;
use clap::ValueEnum;
use log::info;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;

#[cfg(feature = "ui")]
mod basic_term;
#[cfg(feature = "ui")]
mod full_term;
mod null_ui;

#[derive(Parser, Debug, Clone)]
pub(crate) struct UiArgs {
    /// What kind of user interface to use.
    #[clap(long, default_value = "full")]
    ui: Kind,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum Kind {
    None,
    #[cfg(feature = "ui")]
    Basic,
    #[cfg(feature = "ui")]
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
    checker: &Arc<Mutex<Checker>>,
    problem_store: ProblemStoreRef,
    crate_index: Arc<CrateIndex>,
    event_receiver: Receiver<AppEvent>,
    abort_sender: Sender<()>,
) -> Result<JoinHandle<Result<()>>> {
    let mut ui: Box<dyn UserInterface> = match args.ui_kind() {
        Kind::None => {
            info!("Starting null UI");
            Box::new(null_ui::NullUi::new(args, abort_sender))
        }
        #[cfg(feature = "ui")]
        Kind::Basic => {
            info!("Starting basic terminal UI");
            Box::new(basic_term::BasicTermUi::new(config_path.to_owned()))
        }
        #[cfg(feature = "ui")]
        Kind::Full => {
            info!("Starting full terminal UI");
            Box::new(full_term::FullTermUi::new(
                config_path.to_owned(),
                checker,
                crate_index,
                abort_sender,
            )?)
        }
    };
    Ok(std::thread::Builder::new()
        .name("UI".to_owned())
        .spawn(move || ui.run(problem_store, event_receiver))?)
}

impl Args {
    pub(crate) fn should_capture_cargo_output(&self) -> bool {
        !matches!(self.ui_kind(), Kind::None)
    }

    fn ui_kind(&self) -> Kind {
        match &self.command {
            Command::Check => Kind::None,
            #[cfg(feature = "ui")]
            Command::Ui(ui_args) => ui_args.ui,
            Command::Summary(..) => Kind::None,
            Command::Cargo(..) => Kind::None,
            Command::ProxyBin(..) => Kind::None,
        }
    }
}
