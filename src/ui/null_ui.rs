//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use crate::events::AppEvent;
use crate::problem::Severity;
use crate::problem_store::ProblemStoreRef;
use crate::Args;
use anyhow::Result;
use colored::Colorize;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

pub(crate) struct NullUi {
    args: Arc<Args>,
}

impl NullUi {
    pub(crate) fn new(args: &Arc<Args>) -> Self {
        Self { args: args.clone() }
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
                    let mut has_errors = false;
                    for (_, problem) in pstore.deduplicated_into_iter() {
                        let severity = if self.args.fail_on_warnings {
                            Severity::Error
                        } else {
                            problem.severity()
                        };
                        match severity {
                            Severity::Warning => {
                                println!("{} {problem:#}", "WARNING:".yellow())
                            }
                            Severity::Error => {
                                has_errors = true;
                                println!("{} {problem:#}", "ERROR:".red())
                            }
                        }
                    }
                    if has_errors {
                        pstore.abort();
                    } else {
                        loop {
                            let maybe_index = pstore
                                .iterate_with_duplicates()
                                .next()
                                .map(|(index, _)| index);
                            if let Some(index) = maybe_index {
                                pstore.resolve(index);
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[test]
fn test_null_ui_with_warning() {
    use crate::problem::Problem::UnusedPackageConfig;

    let mut ui = NullUi::new(&Arc::new(Args::default()));
    let (event_send, event_recv) = std::sync::mpsc::channel();
    let mut problem_store = crate::problem_store::create(event_send.clone());
    let join_handle = std::thread::spawn({
        let problem_store = problem_store.clone();
        move || {
            crate::ui::UserInterface::run(&mut ui, problem_store, event_recv).unwrap();
        }
    });
    let mut problems = crate::problem::ProblemList::default();
    problems.push(UnusedPackageConfig("crab1".to_owned()));
    problems.push(UnusedPackageConfig("crab2".to_owned()));
    let outcome = problem_store.fix_problems(problems);
    assert_eq!(outcome, crate::outcome::Outcome::Continue);
    event_send.send(AppEvent::Shutdown).unwrap();
    join_handle.join().unwrap();
}
