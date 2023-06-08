use super::FixOutcome;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::Event;
use crossterm::event::KeyCode;
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
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use ratatui::Terminal;
use std::collections::VecDeque;
use std::io::Stdout;
use std::path::PathBuf;

use crate::config_editor;
use crate::config_editor::ConfigEditor;
use crate::config_editor::Edit;
use crate::problem::Problems;

pub(crate) struct FullTermUi {
    config_path: PathBuf,
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl FullTermUi {
    pub(crate) fn new(config_path: PathBuf) -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;
        Ok(Self {
            config_path,
            terminal,
        })
    }
}

impl super::Ui for FullTermUi {
    fn maybe_fix_problems(&mut self, problems: &Problems) -> anyhow::Result<FixOutcome> {
        let problems = problems.clone().grouped_by_type_crate_and_api();
        let mut problems_ui = ProblemsUi::new(problems, self.config_path.clone());

        loop {
            match problems_ui.mode {
                Mode::Quit => return Ok(FixOutcome::GiveUp),
                Mode::Continue => return Ok(FixOutcome::Retry),
                _ => {}
            }
            self.terminal.draw(|f| {
                if let Err(error) = problems_ui.render(f) {
                    problems_ui.error = Some(error);
                }
                problems_ui.render_error(f);
            })?;
            match crossterm::event::read() {
                Ok(event) => {
                    if let Err(error) = problems_ui.handle_event(event) {
                        problems_ui.error = Some(error);
                    }
                }
                Err(_) => break,
            }
        }
        Ok(FixOutcome::GiveUp)
    }

    fn create_initial_config(&mut self) -> anyhow::Result<()> {
        todo!()
    }
}

impl Drop for FullTermUi {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

struct ProblemsUi {
    problems: Problems,
    mode: Mode,
    problem_index: usize,
    edit_index: usize,
    config_path: PathBuf,
    error: Option<anyhow::Error>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectProblem,
    SelectEdit,
    Quit,
    Continue,
}

impl ProblemsUi {
    fn new(problems: Problems, config_path: PathBuf) -> Self {
        Self {
            problems,
            mode: Mode::SelectProblem,
            problem_index: 0,
            edit_index: 0,
            config_path,
            error: None,
        }
    }

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .margin(1)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(f.size());

        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(horizontal[0]);

        self.render_problems(f, left[0]);
        self.render_details(f, left[1]);

        match self.mode {
            Mode::SelectProblem => {}
            Mode::SelectEdit => self.render_edits_and_diff(f, horizontal[1])?,
            Mode::Quit | Mode::Continue => {}
        }
        Ok(())
    }

    fn render_error(&self, f: &mut Frame<CrosstermBackend<Stdout>>) {
        let Some(error) = self.error.as_ref() else { return; };
        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Percentage(25),
                Constraint::Percentage(50),
                Constraint::Percentage(25),
            ])
            .split(f.size());

        let horizontal_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![
                Constraint::Percentage(10),
                Constraint::Percentage(80),
                Constraint::Percentage(10),
            ])
            .split(vertical_chunks[1]);
        let area = horizontal_chunks[1];

        let block = Block::default()
            .title("Error")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red));
        let paragraph = Paragraph::new(format!("{error:#}"))
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(Clear, area);
        f.render_widget(paragraph, area);
    }

    fn render_list(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        title: &str,
        items: impl Iterator<Item = String>,
        active: bool,
        area: Rect,
        index: usize,
    ) {
        let items: Vec<_> = items.map(ListItem::new).collect();
        let mut block = Block::default().title(title).borders(Borders::ALL);
        if active {
            block = block
                .border_type(BorderType::Thick)
                .border_style(Style::default().fg(Color::Yellow));
        }
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut list_state = ListState::default();
        list_state.select(Some(index));
        f.render_stateful_widget(list, area, &mut list_state);
    }

    fn render_problems(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let items = self
            .problems
            .into_iter()
            .map(|problem| problem.short_description());
        self.render_list(
            f,
            "Problems",
            items,
            self.mode == Mode::SelectProblem,
            area,
            self.problem_index,
        );
    }

    fn render_details(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let block = Block::default().title("Details").borders(Borders::ALL);
        let paragraph = Paragraph::new(self.problems[self.problem_index].details())
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn render_edits_and_diff(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: Rect,
    ) -> Result<()> {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let edits = self.edits();
        self.render_edit_selector(&edits, f, chunks[0]);
        self.render_diff(&edits, f, chunks[1])?;
        Ok(())
    }

    fn edits(&self) -> Vec<Box<dyn Edit>> {
        let problem = &self.problems[self.problem_index];
        config_editor::fixes_for_problem(problem)
    }

    fn render_edit_selector(
        &self,
        edits: &[Box<dyn Edit>],
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: Rect,
    ) {
        let items = edits.iter().map(|fix| fix.title());
        self.render_list(
            f,
            "Edits",
            items,
            self.mode == Mode::SelectEdit,
            area,
            self.edit_index,
        );
    }

    fn render_diff(
        &self,
        edits: &[Box<dyn Edit>],
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: Rect,
    ) -> Result<()> {
        let Some(edit) = edits.get(self.edit_index) else {
            return Ok(());
        };

        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        edit.apply(&mut editor)?;
        let original = std::fs::read_to_string(&self.config_path)?;
        let updated = editor.to_toml();

        const CONTEXT: usize = 2;
        let mut common = VecDeque::new();
        let mut after_context = 0;
        let mut lines = Vec::new();
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

        let block = Block::default().title("Config diff").borders(Borders::ALL);
        let paragraph = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        Ok(())
    }

    fn handle_event(&mut self, event: crossterm::event::Event) -> Result<()> {
        let Event::Key(key) = event else {
            return Ok(());
        };
        // When we're displaying an error, any key will dismiss the error popup. They key should
        // then be ignored.
        if self.error.take().is_some() {
            return Ok(());
        }
        match (self.mode, key.code) {
            (_, KeyCode::Char('q')) => self.mode = Mode::Quit,
            (Mode::SelectProblem, KeyCode::Up | KeyCode::Down) => {
                update_counter(&mut self.problem_index, key.code, self.problems.len());
            }
            (Mode::SelectEdit, KeyCode::Up | KeyCode::Down) => {
                let num_edits = self.edits().len();
                update_counter(&mut self.edit_index, key.code, num_edits);
            }
            (Mode::SelectProblem, KeyCode::Char(' ')) => {
                self.mode = Mode::SelectEdit;
                self.edit_index = 0;
            }
            (Mode::SelectEdit, KeyCode::Char(' ')) => {
                self.apply_selected_edit()?;
                self.problems.remove(self.problem_index);
                if self.problem_index >= self.problems.len() {
                    self.problem_index = 0;
                }
                if self.problems.is_empty() {
                    self.mode = Mode::Continue;
                } else {
                    self.mode = Mode::SelectProblem;
                }
            }
            (_, KeyCode::Esc) => self.mode = Mode::SelectProblem,
            _ => {}
        }
        Ok(())
    }

    fn apply_selected_edit(&self) -> Result<()> {
        let edits = &self.edits();
        let edit = edits
            .get(self.edit_index)
            .ok_or_else(|| anyhow!("Selected edit out of range"))?;
        let mut editor = ConfigEditor::from_file(&self.config_path)?;
        edit.apply(&mut editor)?;
        std::fs::write(&self.config_path, editor.to_toml())
            .with_context(|| format!("Failed to write `{}`", self.config_path.display()))
    }
}

/// Increment or decrement `counter`, wrapping at `len`. `keycode` must be Down or Up.
fn update_counter(counter: &mut usize, key_code: KeyCode, len: usize) {
    match key_code {
        KeyCode::Up => *counter = (*counter + len - 1) % len,
        KeyCode::Down => *counter = (*counter + len + 1) % len,
        _ => panic!("Invalid call to update_counter"),
    }
}
