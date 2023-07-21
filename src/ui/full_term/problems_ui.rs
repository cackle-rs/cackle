//! Terminal user interface for showing and resolving detected problems.

use super::message_area;
use super::render_list;
use super::update_counter;
use crate::checker::ApiUsage;
use crate::config::CrateName;
use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::crate_index::CrateIndex;
use crate::crate_index::PackageId;
use crate::location::SourceLocation;
use crate::problem::Problem;
use crate::problem_store::ProblemStore;
use crate::problem_store::ProblemStoreIndex;
use crate::problem_store::ProblemStoreRef;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
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
use std::rc::Rc;
use std::sync::Arc;
use std::sync::MutexGuard;

mod diff;

pub(super) struct ProblemsUi {
    problem_store: ProblemStoreRef,
    crate_index: Arc<CrateIndex>,
    modes: Vec<Mode>,
    problem_index: usize,
    edit_index: usize,
    usage_index: usize,
    config_path: PathBuf,
    accept_single_enabled: bool,
    show_package_details: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectProblem,
    SelectEdit,
    SelectUsage,
    PromptAutoAccept,
    ShowPackageTree,
    Help,
}

impl ProblemsUi {
    pub(super) fn quit_requested(&self) -> bool {
        self.modes.is_empty()
    }

    pub(super) fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) {
        let chunks = if self.show_package_details {
            split_vertial(f.size(), &[40, 40, 20])
        } else {
            split_vertial(f.size(), &[50, 50])
        };
        let (top, middle) = (chunks[0], chunks[1]);

        self.render_problems(f, top);
        if let Some(bottom) = chunks.get(2) {
            self.render_package_details(f, *bottom);
        }

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
                        self.render_details(f, middle);
                    }
                }
                Mode::SelectEdit => {
                    self.render_edit_help_and_diff(f, middle);
                }
                Mode::SelectUsage => {
                    self.render_usage_details(f, middle);
                }
                Mode::PromptAutoAccept => render_auto_accept(f),
                Mode::ShowPackageTree => self.render_package_tree(f),
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
                self.enter_edit_mode();
            }
            (Mode::SelectProblem | Mode::SelectEdit, KeyCode::Char('d')) => {
                if self.usages().is_empty() {
                    bail!("Sorry. No additional details available for this problem");
                }
                self.enter_usage_mode();
            }
            (Mode::SelectProblem, KeyCode::Char('t')) => {
                self.modes.push(Mode::ShowPackageTree);
            }
            (Mode::ShowPackageTree, _) => {
                self.modes.pop();
            }
            (Mode::SelectUsage, KeyCode::Char('d')) => {
                // We're already in details mode, drop back out to the problems list.
                self.modes.pop();
            }
            (Mode::SelectUsage, KeyCode::Char('f')) => {
                // We're showing details, jump over to showing edits.
                self.modes.pop();
                self.enter_edit_mode();
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
            (_, KeyCode::Char('p')) => {
                self.show_package_details = !self.show_package_details;
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

    fn enter_usage_mode(&mut self) {
        self.modes.push(Mode::SelectUsage);
        self.usage_index = 0;
    }

    fn enter_edit_mode(&mut self) {
        self.modes.push(Mode::SelectEdit);
        self.edit_index = 0;
    }

    pub(super) fn new(
        problem_store: ProblemStoreRef,
        crate_index: Arc<CrateIndex>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            problem_store,
            crate_index,
            modes: vec![Mode::SelectProblem],
            problem_index: 0,
            edit_index: 0,
            usage_index: 0,
            config_path,
            accept_single_enabled: false,
            show_package_details: true,
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
                    let usages =
                        usages_for_problem(pstore_lock, self.problem_index, &self.crate_index);
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
            .map(|(_, problem)| problem_details(problem))
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
        usages_for_problem(
            &self.problem_store.lock(),
            self.problem_index,
            &self.crate_index,
        )
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

    fn render_package_details(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        use std::fmt::Write;

        let Some(pkg_id) = self.current_package_id() else {
            return;
        };
        let pkg_name = CrateName::from(&pkg_id);
        let mut text = String::new();
        if let Some(crate_info) = self.crate_index.package_info(&pkg_id) {
            if let Some(description) = &crate_info.description {
                writeln!(&mut text, "Description: {}", description.trim_end()).unwrap();
            }
            writeln!(&mut text, "Version: {}", pkg_id.version()).unwrap();
            if let Some(documentation) = &crate_info.documentation {
                writeln!(&mut text, "Documentation: {documentation}").unwrap();
            }
            writeln!(&mut text, "Local path: {}", crate_info.directory).unwrap();
        }

        let block = Block::default()
            .title(format!("Details for package {pkg_name}"))
            .borders(Borders::ALL);
        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn render_package_tree(&self, f: &mut Frame<CrosstermBackend<Stdout>>) {
        let text = self
            .package_tree_text()
            .unwrap_or_else(|error| error.to_string());
        let lines: Vec<_> = text.lines().collect();
        render_message(f, None, &lines);
    }

    fn package_tree_text(&self) -> Result<String> {
        let pkg_id = self
            .current_package_id()
            .ok_or_else(|| anyhow!("No package selected"))?;
        let output = std::process::Command::new("cargo")
            .arg("tree")
            .arg("--manifest-path")
            .arg(&self.crate_index.manifest_path)
            .arg("-i")
            .arg(pkg_id.name())
            .output()
            .context("Failed to run `cargo tree`")?;
        let mut text =
            String::from_utf8(output.stdout).context("cargo tree produced invalid UTF-8")?;
        if let Ok(stderr) = std::str::from_utf8(&output.stderr) {
            text.push_str(stderr);
        }
        Ok(text)
    }

    fn current_package_id(&self) -> Option<PackageId> {
        let pstore = &self.problem_store.lock();
        let Some((_, problem)) = pstore.deduplicated_into_iter().nth(self.problem_index) else {
            return None;
        };
        problem.pkg_id().cloned()
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
    lines.push(Line::from(edit.help().to_string()));
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
    let before_context = 2;
    let max_lines = 5;

    let mut lines = Vec::new();
    let source_location = usage.source_location();
    lines.push(Line::from(format!(
        "{}",
        source_location.filename().display()
    )));

    let source = crate::fs::read_to_string(source_location.filename())?;
    let target_line = source_location.line() as i32;
    let start_line = (target_line - before_context).max(1);
    let gutter_width = ((start_line + max_lines as i32).ilog10() + 1) as usize;
    for (n, line) in source.lines().skip(start_line as usize - 1).enumerate() {
        if n == max_lines {
            break;
        }
        let line_number = start_line + n as i32;
        let marker = if line_number == target_line {
            "> "
        } else {
            "  "
        };
        let mut spans = vec![Span::from(format!(
            "{marker}{:gutter_width$}: ",
            line_number
        ))];
        if line_number == target_line {
            if let Some(column) = source_location.column() {
                let column = column as usize - 1;
                spans.push(Span::from(line[..column].to_owned()));
                spans.push(Span::styled(
                    line[column..column + 1].to_owned(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::from(line[column + 1..].to_owned()));
            } else {
                spans.push(Span::styled(
                    line.to_owned(),
                    Style::default().add_modifier(Modifier::UNDERLINED),
                ));
            }
        } else {
            spans.push(Span::from(line.to_owned()));
        }
        lines.push(Line::from(spans));
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
                    ("t", "Show tree of crate dependencies to this crate"),
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
                    ("d", "Jump to usage details (API/unsafe only)"),
                    ("up", "Select previous edit"),
                    ("down", "Select next edit"),
                    ("esc", "Return to problem list"),
                ]
                .into_iter(),
            );
        }
        Some(Mode::SelectUsage) => {
            title = "Help for select-usage";
            keys.extend(
                [
                    ("up", "Select previous usage"),
                    ("down", "Select next usage"),
                    ("f", "Jump to edits for the current problem"),
                    ("d/esc", "Return to problem list"),
                ]
                .into_iter(),
            );
        }
        _ => {}
    }
    keys.extend(
        [
            ("p", "Toggle display of package details"),
            ("q", "Quit"),
            ("h/?", "Show mode-specific help"),
        ]
        .into_iter(),
    );
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
    crate_index: &CrateIndex,
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
                let pkg_dir = crate_index
                    .pkg_dir(unsafe_usage.crate_sel.pkg_id())
                    .map(|pkg_dir| pkg_dir.to_owned());
                usages_out.push(Box::new(UnsafeLocation {
                    source_location: location.clone(),
                    pkg_dir,
                }));
            }
        }
        _ => (),
    }
    usages_out.sort_by_key(|u| u.source_location().clone());
    usages_out
}

struct UnsafeLocation {
    source_location: SourceLocation,
    pkg_dir: Option<PathBuf>,
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
        format!("{} -> {}", self.from, self.to_source)
    }
}

impl DisplayUsage for UnsafeLocation {
    fn source_location(&self) -> &SourceLocation {
        &self.source_location
    }

    fn list_display(&self) -> String {
        // In the list, we'd prefer to display source filenames relative to the package root where
        // possible. We already know the crate name and all the usage locations for a crate will
        // generally be under the package root. For any that aren't, we fall back to using the full
        // filename.
        let filename = self
            .pkg_dir
            .as_ref()
            .and_then(|pkg_dir| self.source_location.filename().strip_prefix(pkg_dir).ok())
            .unwrap_or_else(|| self.source_location.filename());
        format!("{}:{}", filename.display(), self.source_location.line())
    }
}

fn problem_details(problem: &Problem) -> String {
    if matches!(
        problem,
        Problem::DisallowedUnsafe(..) | Problem::DisallowedApiUsage(..)
    ) {
        "Press 'd' to see details of each usage".to_owned()
    } else {
        // For kinds of problems that don't support per-usage details, show the full details report.
        format!("{problem:#}")
    }
}

fn split_vertial(area: Rect, percentages: &[u16]) -> Rc<[Rect]> {
    let constraints: Vec<_> = percentages
        .iter()
        .cloned()
        .map(Constraint::Percentage)
        .collect();
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area)
}
