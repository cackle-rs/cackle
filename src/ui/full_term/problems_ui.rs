//! Terminal user interface for showing and resolving detected problems.

use super::message_area;
use super::render_list;
use super::split_vertical;
use super::update_counter;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::problem_store::ProblemStore;
use crate::problem_store::ProblemStoreIndex;
use crate::problem_store::ProblemStoreRef;
use anyhow::bail;
use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::ListItem;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::MutexGuard;

mod diff;

pub(super) struct ProblemsUi {
    problem_store: ProblemStoreRef,
    modes: Vec<Mode>,
    problem_index: usize,
    edit_index: usize,
    config_path: PathBuf,
    accept_single_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectProblem,
    SelectEdit,
    PromptAutoAccept,
}

impl ProblemsUi {
    pub(super) fn quit_requested(&self) -> bool {
        self.modes.is_empty()
    }

    pub(super) fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        if self.problem_store.lock().is_empty() {
            super::render_build_progress(f);
            return Ok(());
        }

        let (top_left, bottom_left) = split_vertical(f.size());

        self.render_problems(f, top_left);

        for mode in self.modes.iter() {
            match mode {
                Mode::SelectProblem => {
                    // If we're selecting an edit, then we don't show details, since they both use
                    // the same area.
                    if !self.modes.contains(&Mode::SelectEdit) {
                        self.render_details(f, bottom_left);
                    }
                }
                Mode::SelectEdit => {
                    self.render_edit_help_and_diff(f, bottom_left)?;
                }
                Mode::PromptAutoAccept => render_auto_accept(f),
            }
        }
        Ok(())
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(mode) = self.modes.last() else {
            return Ok(());
        };
        match (mode, key.code) {
            (_, KeyCode::Char('q')) => self.modes.clear(),
            (Mode::SelectProblem, KeyCode::Up | KeyCode::Down) => {
                update_counter(
                    &mut self.problem_index,
                    key.code,
                    self.problem_store.lock().len(),
                );
            }
            (Mode::SelectEdit, KeyCode::Up | KeyCode::Down) => {
                let num_edits = self.edits().len();
                update_counter(&mut self.edit_index, key.code, num_edits);
            }
            (Mode::SelectProblem, KeyCode::Char(' ') | KeyCode::Enter) => {
                if self.edits().is_empty() {
                    bail!("Sorry. No automatic edits exist for this problem");
                }
                self.modes.push(Mode::SelectEdit);
                self.edit_index = 0;
            }
            (Mode::SelectEdit, KeyCode::Char(' ') | KeyCode::Enter) => {
                self.apply_selected_edit()?;
                if self.problem_index >= self.problem_store.lock().len() {
                    self.problem_index = 0;
                }
                self.modes.pop();
            }
            (Mode::SelectProblem, KeyCode::Char('a')) => {
                if !self.accept_single_enabled {
                    self.modes.push(Mode::PromptAutoAccept);
                }
            }
            (Mode::PromptAutoAccept, KeyCode::Enter) => {
                self.accept_single_enabled = true;
                self.accept_all_single_edits()?;
                self.modes.pop();
            }
            (_, KeyCode::Esc) => {
                if self.modes.len() >= 2 {
                    self.modes.pop();
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn new(problem_store: ProblemStoreRef, config_path: PathBuf) -> Self {
        Self {
            problem_store,
            modes: vec![Mode::SelectProblem],
            problem_index: 0,
            edit_index: 0,
            config_path,
            accept_single_enabled: false,
        }
    }

    pub(super) fn problems_added(&mut self) -> Result<()> {
        if self.accept_single_enabled {
            self.accept_all_single_edits()?;
        }
        Ok(())
    }

    fn accept_all_single_edits(&mut self) -> Result<()> {
        fn first_single_edit(
            pstore: &MutexGuard<ProblemStore>,
        ) -> Option<(ProblemStoreIndex, Box<dyn Edit>)> {
            pstore.into_iter().find_map(|(index, problem)| {
                let mut edits = config_editor::fixes_for_problem(problem);
                if edits.len() == 1 {
                    Some((index, edits.pop().unwrap()))
                } else {
                    None
                }
            })
        }

        let mut pstore = self.problem_store.lock();
        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        while let Some((index, edit)) = first_single_edit(&pstore) {
            edit.apply(&mut editor)?;
            pstore.resolve(index);
        }
        self.write_config(&editor)?;
        Ok(())
    }

    fn write_config(&self, editor: &ConfigEditor) -> Result<(), anyhow::Error> {
        crate::fs::write_atomic(&self.config_path, &editor.to_toml())
    }

    fn render_problems(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let pstore_lock = &self.problem_store.lock();
        let mut items = Vec::new();
        let is_edit_mode = self.modes.contains(&Mode::SelectEdit);
        for (index, (_, problem)) in pstore_lock.into_iter().enumerate() {
            items.push(ListItem::new(format!("{problem}")));
            if is_edit_mode && index == self.problem_index {
                let edits = edits_for_problem(pstore_lock, self.problem_index);
                items.extend(
                    edits
                        .iter()
                        .map(|fix| ListItem::new(format!("  {}", fix.title()))),
                );
            }
        }
        let mut index = self.problem_index;
        if is_edit_mode {
            index += self.edit_index + 1
        }

        render_list(
            f,
            "Problems",
            items.into_iter(),
            matches!(
                self.modes.last(),
                Some(&Mode::SelectProblem | &Mode::SelectEdit)
            ),
            area,
            index,
        );
    }

    fn render_details(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let block = Block::default().title("Details").borders(Borders::ALL);
        let pstore_lock = &self.problem_store.lock();
        let Some((_, problem)) = pstore_lock.into_iter().nth(self.problem_index) else {
            return;
        };
        let paragraph = Paragraph::new(problem.details())
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn edits(&self) -> Vec<Box<dyn Edit>> {
        edits_for_problem(&self.problem_store.lock(), self.problem_index)
    }

    fn render_edit_help_and_diff(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: Rect,
    ) -> Result<()> {
        let edits = self.edits();
        let Some(edit) = edits.get(self.edit_index) else {
            return Ok(());
        };

        let original = std::fs::read_to_string(&self.config_path).unwrap_or_default();
        let mut editor = ConfigEditor::from_toml_string(&original)?;
        edit.apply(&mut editor)?;
        let updated = editor.to_toml();

        let mut lines = Vec::new();
        lines.push(Line::from(edit.help()));

        let mut diff = diff::diff_lines(&original, &updated);

        if !diff.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("=== Diff of cackle.toml ==="));
        }

        lines.append(&mut diff);

        let block = Block::default().title("Edit details").borders(Borders::ALL);
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        Ok(())
    }

    /// Applies the currently selected edit and resolves the problem that produced that edit.
    fn apply_selected_edit(&self) -> Result<()> {
        let mut pstore_lock = self.problem_store.lock();
        let edits = edits_for_problem(&pstore_lock, self.problem_index);
        let Some(edit) = edits.get(self.edit_index) else {
            return Ok(());
        };
        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        edit.apply(&mut editor)?;
        self.write_config(&editor)?;

        // Resolve the currently selected problem.
        if let Some((index, _)) = pstore_lock.into_iter().nth(self.problem_index) {
            pstore_lock.replace(index, edit.replacement_problems());
        }

        // Resolve any other problems that now have no-op edits.
        pstore_lock.resolve_problems_with_empty_diff(&editor);
        Ok(())
    }
}

fn render_auto_accept(f: &mut Frame<CrosstermBackend<Stdout>>) {
    let area = message_area(f.size());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let raw_lines = [
        "Auto-accept edits for all problems that only have a single edit?",
        "It's recommended that you look over the resulting cackle.toml afterwards to see if there are any crates with permissions that you don't think they should have.",
        "Press enter to accept, or escape to cancel.",
    ];
    let mut lines = Vec::new();
    for l in raw_lines {
        lines.push(Line::from(l));
        lines.push(Line::from(""));
    }
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn edits_for_problem(
    pstore_lock: &MutexGuard<ProblemStore>,
    problem_index: usize,
) -> Vec<Box<dyn Edit>> {
    let Some((_, problem)) = pstore_lock.into_iter().nth(problem_index) else {
        return Vec::new();
    };
    config_editor::fixes_for_problem(problem)
}
