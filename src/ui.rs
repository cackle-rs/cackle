//! User interface for showing problems to the user and asking them what they'd like to do about
//! them.

use crate::config_editor::ConfigEditor;
use crate::problem::Problems;
use anyhow::Result;
use colored::Colorize;
use indoc::indoc;
use std::collections::VecDeque;
use std::path::PathBuf;

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
}

impl BasicTermUi {
    pub(crate) fn new(config_path: PathBuf) -> Self {
        Self { config_path }
    }
}

impl Ui for BasicTermUi {
    fn maybe_fix_problems(&mut self, problems: &Problems) -> Result<FixOutcome> {
        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        let fixable = editor.fix_problems(&problems)?;
        println!();
        if fixable.is_empty() {
            for problem in problems {
                println!("{problem}");
            }
        } else {
            for problem in &fixable {
                println!("{problem}");
            }
        }
        loop {
            if fixable.is_empty() {
                println!("Retry or skip? [?/r/s]");
            } else {
                println!("Retry, skip, fix or diff? [?/r/s/f/d]");
            }

            let mut response = String::new();
            std::io::stdin().read_line(&mut response)?;
            match response.trim().to_lowercase().as_str() {
                "f" => {
                    // We always recompute the edits in case the user manually edited the file.
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    editor.fix_problems(&problems)?;
                    editor.write(&self.config_path)?;
                    return Ok(FixOutcome::Retry);
                }
                "d" => {
                    let mut editor = ConfigEditor::from_file(&self.config_path)?;
                    editor.fix_problems(&problems)?;
                    show_diff(
                        &std::fs::read_to_string(&self.config_path)?,
                        &editor.to_toml(),
                    );
                }
                "r" => return Ok(FixOutcome::Retry),
                "s" => return Ok(FixOutcome::GiveUp),
                _ => {
                    print_help(!fixable.is_empty());
                }
            }
        }
    }
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

fn print_help(has_fixable: bool) {
    println!(indoc! {r#"
        r   Retry (e.g. if you've manually edited cackle.toml)
        s   Skip
    "#});
    if !has_fixable {
        return;
    }
    println!(indoc! {r#"
        f   Fix problems by applying automatic edits to cackle.toml
        d   Show diff of automatic edits that would be applied to cackle.toml
    "#});
}
