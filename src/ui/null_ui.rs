//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use crate::events::AppEvent;
use crate::problem::Severity;
use crate::problem_store::ProblemStoreRef;
use crate::Args;
use anyhow::Result;
use colored::Colorize;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;

pub(crate) struct NullUi {
    args: Arc<Args>,
    abort_sender: Sender<()>,
}

impl NullUi {
    pub(crate) fn new(args: &Arc<Args>, abort_sender: Sender<()>) -> Self {
        Self {
            args: args.clone(),
            abort_sender,
        }
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
                    let mut has_errors = false;
                    for (_, problem) in pstore.deduplicated_into_iter() {
                        let mut severity = problem.severity();
                        if self.args.command.is_some() && severity == Severity::Warning {
                            // When running for example `cackle test`, not everything will be
                            // analysed, so unused warnings are expected. As such, we supress all
                            // warnings.
                            continue;
                        }
                        if self.args.fail_on_warnings {
                            severity = Severity::Error
                        };
                        match severity {
                            Severity::Warning => {
                                println!("{} {problem:#}", "WARNING:".yellow())
                            }
                            Severity::Error => {
                                if !has_errors {
                                    has_errors = true;
                                    // Kill cargo process then wait a bit for any terminal output to
                                    // settle before we start reporting errors.
                                    let _ = self.abort_sender.send(());
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                    println!();
                                }
                                println!("{} {problem:#}", "ERROR:".red())
                            }
                        }
                    }
                    if has_errors {
                        pstore.abort();
                    } else {
                        loop {
                            let maybe_index = pstore
                                .deduplicated_into_iter()
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
    use crate::config::permissions::PermSel;
    use crate::problem::Problem::UnusedPackageConfig;

    let (abort_sender, _abort_recv) = std::sync::mpsc::channel();
    let mut ui = NullUi::new(&Arc::new(Args::default()), abort_sender);
    let (event_send, event_recv) = std::sync::mpsc::channel();
    let mut problem_store = crate::problem_store::create(event_send.clone());
    let join_handle = std::thread::spawn({
        let problem_store = problem_store.clone();
        move || {
            crate::ui::UserInterface::run(&mut ui, problem_store, event_recv).unwrap();
        }
    });
    let mut problems = crate::problem::ProblemList::default();
    problems.push(UnusedPackageConfig(PermSel::for_primary("crab1")));
    problems.push(UnusedPackageConfig(PermSel::for_primary("crab2")));
    let outcome = problem_store.fix_problems(problems);
    assert_eq!(outcome, crate::outcome::Outcome::Continue);
    event_send.send(AppEvent::Shutdown).unwrap();
    join_handle.join().unwrap();
}
