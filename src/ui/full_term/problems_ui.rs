//! Terminal user interface for showing and resolving detected problems.

use super::message_area;
use super::render_list;
use super::split_vertical;
use super::update_counter;
use crate::checker::ApiUsage;
use crate::checker::SourceLocation;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::problem::Problem;
use crate::problem_store::ProblemStore;
use crate::problem_store::ProblemStoreIndex;
use crate::problem_store::ProblemStoreRef;
use anyhow::anyhow;
use anyhow::bail;
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
use ratatui::widgets::Clear;
use ratatui::widgets::ListItem;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use std::io::Stdout;
use std::path::Path;
use std::path::PathBuf;
use std::sync::MutexGuard;

mod diff;

pub(super) struct ProblemsUi {
    problem_store: ProblemStoreRef,
    modes: Vec<Mode>,
    problem_index: usize,
    edit_index: usize,
    usage_index: usize,
    config_path: PathBuf,
    accept_single_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectProblem,
    SelectEdit,
    SelectUsage,
    PromptAutoAccept,
    Help,
}

impl ProblemsUi {
    pub(super) fn quit_requested(&self) -> bool {
        self.modes.is_empty()
    }

    pub(super) fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) {
        let (top_left, bottom_left) = split_vertical(f.size());

        self.render_problems(f, top_left);

        let mut previous_mode = None;
        for mode in self.modes.iter() {
            match mode {
                Mode::SelectProblem => {
                    // If we're selecting an edit or a usage, then we don't show details, since they
                    // both use the same area.
                    if !self
                        .modes
                        .iter()
                        .any(|mode| [Mode::SelectEdit, Mode::SelectUsage].contains(mode))
                    {
                        self.render_details(f, bottom_left);
                    }
                }
                Mode::SelectEdit => {
                    self.render_edit_help_and_diff(f, bottom_left);
                }
                Mode::SelectUsage => {
                    self.render_usage_details(f, bottom_left);
                }
                Mode::PromptAutoAccept => render_auto_accept(f),
                Mode::Help => render_help(f, previous_mode),
            }
            previous_mode = Some(mode);
        }
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
            (Mode::SelectUsage, KeyCode::Up | KeyCode::Down) => {
                let num_usages = self.usages().len();
                update_counter(&mut self.usage_index, key.code, num_usages);
            }
            (Mode::SelectProblem, KeyCode::Char('f')) => {
                if self.edits().is_empty() {
                    bail!("Sorry. No automatic edits exist for this problem");
                }
                self.modes.push(Mode::SelectEdit);
                self.edit_index = 0;
            }
            (Mode::SelectProblem, KeyCode::Char('d')) => {
                if self.usages().is_empty() {
                    bail!("Sorry. No additional details available for this problem");
                }
                self.modes.push(Mode::SelectUsage);
                self.usage_index = 0;
            }
            (Mode::SelectEdit, KeyCode::Char(' ' | 'f') | KeyCode::Enter) => {
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
            (_, KeyCode::Char('h' | '?')) => self.modes.push(Mode::Help),
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
            usage_index: 0,
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
            pstore
                .iterate_with_duplicates()
                .find_map(|(index, problem)| {
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
        if pstore_lock.is_empty() {
            super::render_build_progress(f, area);
            return;
        }
        let mut items = Vec::new();
        let is_edit_mode = self.modes.contains(&Mode::SelectEdit);
        let is_usage_mode = self.modes.contains(&Mode::SelectUsage);
        for (index, (_, problem)) in pstore_lock.deduplicated_into_iter().enumerate() {
            items.push(ListItem::new(format!("{problem}")));
            if index == self.problem_index {
                if is_edit_mode {
                    let edits = edits_for_problem(pstore_lock, self.problem_index);
                    items.extend(
                        edits
                            .iter()
                            .map(|fix| ListItem::new(format!("  {}", fix.title()))),
                    );
                } else if is_usage_mode {
                    let usages = usages_for_problem(pstore_lock, self.problem_index);
                    items.extend(
                        usages
                            .iter()
                            .map(|usage| ListItem::new(format!("  {}", usage.list_display()))),
                    );
                }
            }
        }
        let mut index = self.problem_index;
        let title;
        if is_edit_mode {
            title = "Select edit";
            index += self.edit_index + 1
        } else if is_usage_mode {
            title = "Select usage";
            index += self.usage_index + 1
        } else {
            title = "Problems";
        }

        render_list(
            f,
            title,
            items.into_iter(),
            matches!(
                self.modes.last(),
                Some(&Mode::SelectProblem | &Mode::SelectEdit | &Mode::SelectUsage)
            ),
            area,
            index,
        );
    }

    fn render_details(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let block = Block::default().title("Details").borders(Borders::ALL);
        let pstore_lock = &self.problem_store.lock();
        let details = pstore_lock
            .deduplicated_into_iter()
            .nth(self.problem_index)
            .map(|(_, problem)| problem.details())
            .unwrap_or_default();
        let paragraph = Paragraph::new(details)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn edits(&self) -> Vec<Box<dyn Edit>> {
        edits_for_problem(&self.problem_store.lock(), self.problem_index)
    }

    fn usages(&self) -> Vec<Box<dyn DisplayUsage>> {
        usages_for_problem(&self.problem_store.lock(), self.problem_index)
    }

    fn render_edit_help_and_diff(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let edits = self.edits();
        let Some(edit) = edits.get(self.edit_index) else {
            return;
        };

        let lines = config_diff_lines(&self.config_path, &**edit).unwrap_or_else(error_lines);

        let block = Block::default().title("Edit details").borders(Borders::ALL);
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn render_usage_details(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let usages = self.usages();
        let Some(usage) = usages.get(self.usage_index) else {
            return;
        };

        let mut lines = usage_source_lines(&**usage).unwrap_or_else(error_lines);

        if let Some(debug_data) = usage.debug_data() {
            lines.push(Line::from(""));
            for line in debug_data.lines() {
                lines.push(Line::from(line.to_owned()));
            }
        }

        let block = Block::default()
            .title("Usage details")
            .borders(Borders::ALL);
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
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
        let maybe_index = pstore_lock
            .deduplicated_into_iter()
            .nth(self.problem_index)
            .map(|(index, _)| index);
        if let Some(index) = maybe_index {
            pstore_lock.replace(index, edit.replacement_problems());
        }

        // Resolve any other problems that now have no-op edits.
        pstore_lock.resolve_problems_with_empty_diff(&editor);
        Ok(())
    }
}

fn error_lines(error: anyhow::Error) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(
        format!("{error:#}"),
        Style::default().fg(Color::Red),
    ))]
}

fn config_diff_lines(config_path: &Path, edit: &dyn Edit) -> Result<Vec<Line<'static>>> {
    let mut lines = Vec::new();
    lines.push(Line::from(edit.help()));
    let original = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut editor = ConfigEditor::from_toml_string(&original)?;
    if let Err(error) = edit.apply(&mut editor) {
        lines.push(Line::from(""));
        lines.push(Line::from(error.to_string()));
    }
    let updated = editor.to_toml();
    let mut diff = diff::diff_lines(&original, &updated);
    if !diff.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("=== Diff of cackle.toml ==="));
    }
    lines.append(&mut diff);
    Ok(lines)
}

fn usage_source_lines(usage: &dyn DisplayUsage) -> Result<Vec<Line<'static>>> {
    let mut lines = Vec::new();
    let source_location = usage.source_location();
    lines.push(Line::from(format!(
        "{}",
        source_location.filename.display()
    )));
    let source = crate::fs::read_to_string(&source_location.filename)?;
    let relevant_line = source
        .lines()
        .nth(source_location.line as usize - 1)
        .ok_or_else(|| anyhow!("Line number not found in file"))?;
    let gutter_width = 5;
    lines.push(Line::from(format!(
        "{:gutter_width$}: {relevant_line}",
        source_location.line
    )));
    if let Some(column) = source_location.column {
        let column = column as usize + 1;
        lines.push(Line::from(format!("{:gutter_width$}{:column$}^", "", "")));
    }
    Ok(lines)
}

fn render_help(f: &mut Frame<CrosstermBackend<Stdout>>, mode: Option<&Mode>) {
    let mut keys = vec![];
    let mut title = "Help";
    match mode {
        Some(Mode::SelectProblem) => {
            title = "Help for select-problem";
            keys.extend(
                [
                    ("f", "Show available automatic fixes for this problem"),
                    (
                        "d",
                        "Select and show details of each usage (API/unsafe only)",
                    ),
                    ("up", "Select previous problem"),
                    ("down", "Select next problem"),
                    ("a", "Enable auto-apply for problems with only one edit"),
                ]
                .into_iter(),
            );
        }
        Some(Mode::SelectEdit) => {
            title = "Help for select-edit";
            keys.extend(
                [
                    ("space/enter/f", "Apply this edit"),
                    ("up", "Select previous edit"),
                    ("down", "Select next edit"),
                ]
                .into_iter(),
            );
        }
        _ => {}
    }
    keys.extend([("q", "Quit"), ("h/?", "Show mode-specific help")].into_iter());
    let lines: Vec<String> = keys
        .into_iter()
        .map(|(key, action)| format!("{key:14} {action}"))
        .collect();
    render_message(f, Some(title), &lines);
}

fn render_auto_accept(f: &mut Frame<CrosstermBackend<Stdout>>) {
    render_message(f, None, &[
        "Auto-accept edits for all problems that only have a single edit?",
        "",
        "It's recommended that you look over the resulting cackle.toml afterwards to see if there are any crates with permissions that you don't think they should have.",
        "",
        "Press enter to accept, or escape to cancel.",
    ]);
}

fn render_message<S: AsRef<str>>(
    f: &mut Frame<CrosstermBackend<Stdout>>,
    title: Option<&str>,
    raw_lines: &[S],
) {
    let area = message_area(f.size());
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    if let Some(title) = title {
        block = block.title(title);
    }
    let lines: Vec<Line> = raw_lines.iter().map(|l| Line::from(l.as_ref())).collect();
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
    let Some((_, problem)) = pstore_lock.deduplicated_into_iter().nth(problem_index) else {
        return Vec::new();
    };
    config_editor::fixes_for_problem(problem)
}

fn usages_for_problem(
    pstore_lock: &MutexGuard<ProblemStore>,
    problem_index: usize,
) -> Vec<Box<dyn DisplayUsage>> {
    let mut usages_out: Vec<Box<dyn DisplayUsage>> = Vec::new();
    match pstore_lock.deduplicated_into_iter().nth(problem_index) {
        Some((_, Problem::DisallowedApiUsage(usages))) => {
            for usages in usages.usages.values() {
                for usage in usages {
                    usages_out.push(Box::new(usage.clone()));
                }
            }
        }
        Some((_, Problem::DisallowedUnsafe(unsafe_usage))) => {
            for location in &unsafe_usage.locations {
                usages_out.push(Box::new(location.clone()));
            }
        }
        _ => (),
    }
    usages_out
}

/// A trait implemented for things that can display in a usage list.
trait DisplayUsage {
    fn source_location(&self) -> &SourceLocation;

    fn debug_data(&self) -> Option<String> {
        None
    }

    /// A single line that we display in the list of usages.
    fn list_display(&self) -> String;
}

impl DisplayUsage for ApiUsage {
    fn source_location(&self) -> &SourceLocation {
        &self.source_location
    }

    fn debug_data(&self) -> Option<String> {
        self.debug_data
            .as_ref()
            .map(|debug_data| format!("{debug_data:#?}"))
    }

    fn list_display(&self) -> String {
        format!("{} -> {}", self.from, self.to)
    }
}

impl DisplayUsage for SourceLocation {
    fn source_location(&self) -> &SourceLocation {
        self
    }

    fn list_display(&self) -> String {
        self.to_string()
    }
}
