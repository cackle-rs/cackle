//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use anyhow::Result;
use colored::Colorize;
use std::sync::mpsc::Receiver;

pub(crate) struct NullUi;

impl NullUi {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl super::UserInterface for NullUi {
    fn run(
        &mut self,
        problem_store: ProblemStoreRef,
        event_receiver: Receiver<AppEvent>,
    ) -> Result<()> {
        while let Ok(event) = event_receiver.recv() {
            match event {
                AppEvent::Shutdown => return Ok(()),
                AppEvent::ProblemsAdded => {
                    let mut pstore = problem_store.lock();
                    pstore.group_by_crate();
                    for (_, problem) in pstore.into_iter() {
                        println!("{} {problem}", "ERROR:".red());
                    }
                    pstore.abort();
                }
            }
        }
        Ok(())
    }
}
