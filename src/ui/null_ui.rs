//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use std::sync::mpsc::Receiver;

use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use anyhow::Result;

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
                AppEvent::ProblemsAdded => problem_store.lock().abort(),
            }
        }
        Ok(())
    }
}
