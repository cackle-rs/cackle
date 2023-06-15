//! A basic text-based terminal UI. Doesn't use curses, just prints stuff and prompts for what to
//! do.

use crate::config;
use crate::config::PermissionName;
use crate::config::SandboxKind;
use crate::config::MAX_VERSION;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::events::AppEvent;
use crate::problem::Problem;
use crate::problem_store::ProblemStoreRef;
use crate::sandbox;
use crate::ui::FixOutcome;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use colored::Colorize;
use indoc::indoc;
use std::collections::VecDeque;
use std::io::BufRead;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::time::SystemTime;

pub(crate) struct BasicTermUi {
    config_path: PathBuf,
    stdin_recv: Receiver<String>,
    config_last_modified: Option<SystemTime>,
}

impl super::UserInterface for BasicTermUi {
    fn run(
        &mut self,
        problem_store: ProblemStoreRef,
        event_receiver: Receiver<AppEvent>,
    ) -> Result<()> {
        while let Ok(event) = event_receiver.recv() {
            match event {
                AppEvent::Shutdown => return Ok(()),
                AppEvent::ProblemsAdded => {}
            }
            loop {
                let pstore_lock = problem_store.lock();
                let Some((problem_index, problem)) = pstore_lock.into_iter().next() else {
                        break;
                    };
                if matches!(problem, Problem::MissingConfiguration(_)) {
                    drop(pstore_lock);
                    match self.create_initial_config() {
                        Ok(FixOutcome::Continue) => {
                            problem_store.lock().resolve(problem_index);
                        }
                        Ok(FixOutcome::GiveUp) => {}
                        Err(_) => todo!(),
                    }
                    continue;
                }
                println!("{problem}");
                let fixes = config_editor::fixes_for_problem(problem);
                // We don't want to hold the mutex for any significant time, so we drop it now
                // that we're done with `problem`, which was the only thing borrowed from the
                // store. We certainly don't want to hold the lock while we prompt for user
                // input.
                drop(pstore_lock);
                for (index, fix) in fixes.iter().enumerate() {
                    println!("{})  {}", index + 1, fix.title());
                }
                if fixes.is_empty() {
                    println!("No automatic fixes available. Edit config manually to continue.");
                } else {
                    println!("dN) Diff for fix N. e.g 'd1'");
                }
                match self.prompt_for_fix(&fixes)? {
                    FixOutcome::Continue => {
                        problem_store.lock().resolve(problem_index);
                    }
                    FixOutcome::GiveUp => {}
                }
            }
        }
        Ok(())
    }
}

impl BasicTermUi {
    pub(crate) fn new(config_path: PathBuf) -> Self {
        Self {
            config_last_modified: config_modification_time(&config_path),
            config_path,
            stdin_recv: start_stdin_channel(),
        }
    }

    fn create_initial_config(&mut self) -> Result<FixOutcome> {
        println!("Creating initial cackle.toml");
        let mut editor = config_editor::ConfigEditor::initial();
        editor.set_version(MAX_VERSION);
        let sandbox_kind = sandbox::available_kind();
        if sandbox_kind == SandboxKind::Disabled {
            println!(indoc! {r#"
                bwrap (bubblewrap) doesn't seem to be installed, so sandboxing will be disabled.
                If you'd like to sandbox execution of build scripts, press control-c, install
                bubble wrap, then try again. On system with apt, you can run:
                sudo apt install bubblewrap
            "#});
        }
        editor.set_sandbox_kind(sandbox_kind)?;
        let built_ins = config::built_in::get_built_ins();
        println!("Available built-in API definitions:");
        for name in built_ins.keys() {
            println!(" - {name}");
        }
        println!(r#"Select std APIs you'd like to restrict .e.g "fs,net,process""#);
        let mut done = false;
        while !done {
            done = true;
            print_prompt()?;
            for part in self.stdin_recv.recv()?.trim().split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if built_ins.contains_key(&PermissionName::new(part)) {
                    editor.toggle_std_import(part)?;
                } else {
                    println!("Unknown API `{part}`");
                    done = false;
                }
            }
        }
        let initial_toml = editor.to_toml();
        println!("========= Initial configuration =========");
        println!("{initial_toml}");
        println!("=========================================");
        println!("Press enter to write config, or control-c to abort");
        print_prompt()?;
        self.stdin_recv.recv()?;
        std::fs::write(&self.config_path, initial_toml)
            .with_context(|| format!("Failed to write `{}`", self.config_path.display()))?;
        self.config_last_modified = config_modification_time(&self.config_path);
        Ok(FixOutcome::Continue)
    }

    fn prompt_for_fix(&mut self, fixes: &[Box<dyn Edit>]) -> Result<FixOutcome> {
        loop {
            match self.get_action(fixes.len()) {
                Ok(Action::ApplyFix(n)) => {
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    fixes[n].apply(&mut editor)?;
                    editor.write(&self.config_path)?;
                    self.config_last_modified = config_modification_time(&self.config_path);
                    return Ok(FixOutcome::Continue);
                }
                Ok(Action::ShowDiff(n)) => {
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    let fix = &fixes[n];
                    fix.apply(&mut editor)?;
                    println!("Diff for {}:", fix.title());
                    show_diff(
                        &std::fs::read_to_string(&self.config_path)?,
                        &editor.to_toml(),
                    );
                }
                Ok(Action::GiveUp) => return Ok(FixOutcome::GiveUp),
                Ok(Action::Retry) => return Ok(FixOutcome::Continue),
                Err(error) => {
                    println!("{error}")
                }
            }
        }
    }

    fn get_action(&mut self, num_fixes: usize) -> Result<Action> {
        print_prompt()?;

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

fn print_prompt() -> Result<(), anyhow::Error> {
    print!(">> ");
    std::io::stdout().lock().flush()?;
    Ok(())
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
