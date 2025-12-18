//! A user-interface that never prompts. This is used when non-interactive mode is selected.

use crate::checker::Checker;
use crate::config::Config;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::events::AppEvent;
use crate::problem::Severity;
use crate::problem_store::ProblemId;
use crate::problem_store::ProblemStore;
use crate::problem_store::ProblemStoreRef;
use crate::Args;
use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

pub(crate) struct NullUi {
    args: Arc<Args>,
    abort_sender: Sender<()>,
    config_path: PathBuf,
    checker: Arc<Mutex<Checker>>,
}

impl NullUi {
    pub(crate) fn new(
        args: &Arc<Args>,
        abort_sender: Sender<()>,
        config_path: PathBuf,
        checker: &Arc<Mutex<Checker>>,
    ) -> Self {
        Self {
            args: args.clone(),
            abort_sender,
            config_path,
            checker: checker.clone(),
        }
    }

    fn accept_all_single_edits(&self, pstore: &mut MutexGuard<ProblemStore>) -> Result<()> {
        let config = self.checker.lock().unwrap().config.clone();
        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        let mut applied_count = 0;

        loop {
            let edit_to_apply = Self::first_sensible_edit(pstore, &config);
            match edit_to_apply {
                Some((index, edit)) => {
                    edit.apply(&mut editor, &Default::default())?;
                    pstore.resolve(index);
                    applied_count += 1;
                }
                None => break,
            }
        }

        if applied_count > 0 {
            crate::fs::write_atomic(&self.config_path, &editor.to_toml())?;
            println!(
                "{}",
                format!("Auto-accepted {} fix(es)", applied_count).green()
            );
        }

        Ok(())
    }

    fn first_sensible_edit(
        pstore: &MutexGuard<ProblemStore>,
        config: &Config,
    ) -> Option<(ProblemId, Box<dyn Edit>)> {
        pstore
            .deduplicated_into_iter()
            .find_map(|(index, problem)| {
                let edits = config_editor::fixes_for_problem(problem, config);
                // Always pick the first edit - these are ordered with the most sensible option first
                edits.into_iter().next().map(|edit| (index, edit))
            })
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

                    // If auto-accept is enabled, apply all fixes automatically
                    if self.args.auto_accept_fixes {
                        self.accept_all_single_edits(&mut pstore)?;
                    }

                    let mut has_errors = false;
                    for (_, problem) in pstore.deduplicated_into_iter() {
                        let mut severity = problem.severity();
                        if self.args.command.is_some() && severity == Severity::Warning {
                            // When running for example `cackle test`, not everything will be
                            // analysed, so unused warnings are expected. As such, we suppress all
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::permissions::PermSel;
    use crate::crate_index::CrateIndex;
    use crate::problem::Problem::UnusedPackageConfig;
    use crate::tmpdir::TempDir;
    use std::path::PathBuf;

    #[test]
    fn test_null_ui_with_warning() {
        let tmpdir = TempDir::new(None).unwrap();
        let target_dir = tmpdir.path().join("target");
        let config_path = tmpdir.path().join("cackle.toml");
        let sysroot = PathBuf::from("/usr");

        // Create a minimal Cargo.toml with a lib target for CrateIndex
        let cargo_toml_path = tmpdir.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml_path,
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmpdir.path().join("src")).unwrap();
        std::fs::write(tmpdir.path().join("src/lib.rs"), "").unwrap();

        let crate_index = Arc::new(CrateIndex::new(tmpdir.path()).unwrap());
        let args = Arc::new(Args::default());

        let checker = Arc::new(Mutex::new(Checker::new(
            Arc::new(tmpdir),
            target_dir,
            args.clone(),
            sysroot.into(),
            crate_index,
            config_path.clone(),
        )));

        let (abort_sender, _abort_recv) = std::sync::mpsc::channel();
        let mut ui = NullUi::new(&args, abort_sender, config_path, &checker);

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
}
