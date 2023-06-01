//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::problem::Problems;
use anyhow::bail;
use anyhow::Result;
use colored::Colorize;
use std::collections::VecDeque;
use std::io::BufRead;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy)]
pub(crate) enum FixOutcome {
    Retry,
    GiveUp,
}

pub(crate) trait Ui {
    /// Prompt the user to fix the supplied problems.
    fn maybe_fix_problems(&mut self, problems: &Problems) -> Result<FixOutcome>;
}

pub(crate) struct NullUi;

impl Ui for NullUi {
    fn maybe_fix_problems(&mut self, _problems: &Problems) -> Result<FixOutcome> {
        Ok(FixOutcome::GiveUp)
    }
}

pub(crate) struct BasicTermUi {
    config_path: PathBuf,
    stdin_recv: Receiver<String>,
    config_last_modified: Option<SystemTime>,
}

impl BasicTermUi {
    pub(crate) fn new(config_path: PathBuf) -> Self {
        Self {
            config_last_modified: config_modification_time(&config_path),
            config_path,
            stdin_recv: start_stdin_channel(),
        }
    }
}

fn start_stdin_channel() -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin().lock();
        for line in stdin.lines() {
            let Ok(line) = line else {
                break;
            };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

impl Ui for BasicTermUi {
    fn maybe_fix_problems(&mut self, problems: &Problems) -> Result<FixOutcome> {
        // For now, we only fix the first problem, then retry.
        let Some(problem) = problems.into_iter().next() else {
            return Ok(FixOutcome::Retry);
        };
        println!("{problem}");
        let fixes = config_editor::fixes_for_problem(problem);
        for (index, fix) in fixes.iter().enumerate() {
            println!("{})  {}", index + 1, fix.title());
        }
        if !fixes.is_empty() {
            println!("dN) Diff for fix N. e.g 'd1'");
        }
        loop {
            match self.get_action(fixes.len()) {
                Ok(Action::ApplyFix(n)) => {
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    fixes[n].apply(&mut editor)?;
                    editor.write(&self.config_path)?;
                    return Ok(FixOutcome::Retry);
                }
                Ok(Action::ShowDiff(n)) => {
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    fixes[n].apply(&mut editor)?;
                    show_diff(
                        &std::fs::read_to_string(&self.config_path)?,
                        &editor.to_toml(),
                    );
                }
                Ok(Action::GiveUp) => return Ok(FixOutcome::GiveUp),
                Ok(Action::Retry) => return Ok(FixOutcome::Retry),
                Err(error) => {
                    println!("{error}")
                }
            }
        }
    }
}

impl BasicTermUi {
    fn get_action(&mut self, num_fixes: usize) -> Result<Action> {
        std::io::stdout().lock().flush()?;

        // Wait until either the user enters a response line, or the config file gets changed.
        // We poll for config file changes because inotify is relatively heavyweight and we
        // don't need an instant response to a file change.
        let response;
        loop {
            match self.stdin_recv.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    response = line.to_lowercase();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let modified = config_modification_time(&self.config_path);
                    if self.config_last_modified != modified {
                        self.config_last_modified = modified;
                        println!("\nConfig file modified, retrying...");
                        return Ok(Action::Retry);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(Action::GiveUp),
            }
        }
        let response = response.trim();
        if let Some(rest) = response.strip_prefix('d') {
            return Ok(Action::ShowDiff(fix_index(rest, num_fixes)?));
        }
        Ok(Action::ApplyFix(fix_index(response, num_fixes)?))
    }
}

fn fix_index(n_str: &str, num_fixes: usize) -> Result<usize> {
    let n: usize = n_str.parse()?;
    if n < 1 || n > num_fixes {
        bail!("Invalid fix number");
    }
    Ok(n - 1)
}

enum Action {
    Retry,
    GiveUp,
    ApplyFix(usize),
    ShowDiff(usize),
}

fn config_modification_time(config_path: &Path) -> Option<SystemTime> {
    std::fs::metadata(config_path).ok()?.modified().ok()
}

fn show_diff(original: &str, updated: &str) {
    fn print_common(common: &mut VecDeque<&str>) {
        for line in common.drain(..) {
            println!(" {line}");
        }
    }

    const CONTEXT: usize = 2;
    let mut common = VecDeque::new();
    let mut after_context = 0;
    for diff in diff::lines(original, updated) {
        match diff {
            diff::Result::Both(s, _) => {
                if after_context > 0 {
                    after_context -= 1;
                    println!(" {s}");
                } else {
                    common.push_back(s);
                    if common.len() > CONTEXT {
                        common.pop_front();
                    }
                }
            }
            diff::Result::Left(s) => {
                print_common(&mut common);
                println!("{}{}", "-".red(), s.red());
                after_context = CONTEXT;
            }
            diff::Result::Right(s) => {
                print_common(&mut common);
                println!("{}{}", "+".green(), s.green());
                after_context = CONTEXT;
            }
        }
    }
}
