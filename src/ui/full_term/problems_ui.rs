//! Terminal user interface for showing and resolving detected problems.

use super::render_list;
use super::split_vertical;
use super::update_counter;
use super::Screen;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::problem_store::ProblemStore;
use crate::problem_store::ProblemStoreIndex;
use crate::problem_store::ProblemStoreRef;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::ListItem;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use std::collections::VecDeque;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::MutexGuard;

pub(super) struct ProblemsUi {
    problem_store: ProblemStoreRef,
    mode: Mode,
    problem_index: usize,
    edit_index: usize,
    config_path: PathBuf,
    accept_single_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectProblem,
    SelectEdit,
    Quit,
}

impl Screen for ProblemsUi {
    fn quit_requested(&self) -> bool {
        self.mode == Mode::Quit
    }

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        if self.problem_store.lock().is_empty() {
            super::render_build_progress(f);
            return Ok(());
        }

        let (top_left, bottom_left) = split_vertical(f.size());

        self.render_problems(f, top_left);

        match self.mode {
            Mode::SelectProblem => self.render_details(f, bottom_left),
            Mode::SelectEdit => {
                self.render_edit_help_and_diff(f, bottom_left)?;
            }
            Mode::Quit => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match (self.mode, key.code) {
            (_, KeyCode::Char('q')) => self.mode = Mode::Quit,
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
                self.mode = Mode::SelectEdit;
                self.edit_index = 0;
            }
            (Mode::SelectEdit, KeyCode::Char(' ') | KeyCode::Enter) => {
                self.apply_selected_edit()?;
                if self.problem_index >= self.problem_store.lock().len() {
                    self.problem_index = 0;
                }
                self.mode = Mode::SelectProblem;
            }
            (Mode::SelectProblem, KeyCode::Char('a')) => {
                self.accept_single_enabled = true;
                self.accept_all_single_edits()?;
            }
            (_, KeyCode::Esc) => self.mode = Mode::SelectProblem,
            _ => {}
        }
        Ok(())
    }
}

impl ProblemsUi {
    pub(super) fn new(problem_store: ProblemStoreRef, config_path: PathBuf) -> Self {
        Self {
            problem_store,
            mode: Mode::SelectProblem,
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
        std::fs::write(&self.config_path, editor.to_toml())
            .with_context(|| format!("Failed to write `{}`", self.config_path.display()))?;
        Ok(())
    }

    fn render_problems(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let pstore_lock = &self.problem_store.lock();
        let mut items = Vec::new();
        for (index, (_, problem)) in pstore_lock.into_iter().enumerate() {
            items.push(ListItem::new(format!("{problem}")));
            if self.mode == Mode::SelectEdit && index == self.problem_index {
                let edits = edits_for_problem(pstore_lock, self.problem_index);
                items.extend(
                    edits
                        .iter()
                        .map(|fix| ListItem::new(format!("  {}", fix.title()))),
                );
            }
        }
        let mut index = self.problem_index;
        if self.mode == Mode::SelectEdit {
            index += self.edit_index + 1
        }

        render_list(
            f,
            "Problems",
            items.into_iter(),
            matches!(self.mode, Mode::SelectProblem | Mode::SelectEdit),
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

        let mut first = true;
        const CONTEXT: usize = 2;
        let mut common = VecDeque::new();
        let mut after_context = 0;
        let mut add_header = |lines: &mut Vec<Line>| {
            if first {
                lines.push(Line::from(""));
                lines.push(Line::from("=== Diff of cackle.toml ==="));
                first = false;
            }
        };
        for diff in diff::lines(&original, &updated) {
            match diff {
                diff::Result::Both(s, _) => {
                    if after_context > 0 {
                        after_context -= 1;
                        lines.push(Line::from(format!(" {s}")));
                    } else {
                        common.push_back(s);
                        if common.len() > CONTEXT {
                            common.pop_front();
                        }
                    }
                }
                diff::Result::Left(s) => {
                    add_header(&mut lines);
                    {
                        let common: &mut VecDeque<&str> = &mut common;
                        for line in common.drain(..) {
                            lines.push(Line::from(format!(" {line}")));
                        }
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!("-{s}"),
                        Style::default().fg(Color::Red),
                    )]));
                    after_context = CONTEXT;
                }
                diff::Result::Right(s) => {
                    add_header(&mut lines);
                    {
                        let common: &mut VecDeque<&str> = &mut common;
                        for line in common.drain(..) {
                            lines.push(Line::from(format!(" {line}")));
                        }
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!("+{s}"),
                        Style::default().fg(Color::Green),
                    )]));
                    after_context = CONTEXT;
                }
            }
        }

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

fn edits_for_problem(
    pstore_lock: &MutexGuard<ProblemStore>,
    problem_index: usize,
) -> Vec<Box<dyn Edit>> {
    let Some((_, problem)) = pstore_lock.into_iter().nth(problem_index) else {
        return Vec::new();
    };
    config_editor::fixes_for_problem(problem)
}
