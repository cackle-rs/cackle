use super::FixOutcome;
use crate::problem::ProblemList;
use anyhow::Result;
use crossterm::event::Event;
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
use std::io::Stdout;
use std::path::PathBuf;

mod edit_config_ui;
mod problems_ui;

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
    fn maybe_fix_problems(&mut self, problems: &ProblemList) -> anyhow::Result<FixOutcome> {
        let problems = problems.clone().grouped_by_type_crate_and_api();
        let mut problems_ui = problems_ui::ProblemsUi::new(problems, self.config_path.clone());
        problems_ui.run(&mut self.terminal)
    }

    fn create_initial_config(&mut self) -> anyhow::Result<FixOutcome> {
        edit_config_ui::EditConfigUi::new(self.config_path.clone()).run(&mut self.terminal)
    }

    fn report_error(&mut self, error: &anyhow::Error) -> Result<()> {
        ErrorScreen::new(error).run(&mut self.terminal)
    }

    fn display_message(&mut self, title: &str, message: &str) -> Result<()> {
        MessageScreen::new(title, message).run(&mut self.terminal)
    }
}

trait Screen {
    type ExitStatus;

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()>;
    fn handle_key(&mut self, key: KeyEvent) -> Result<()>;
    fn exit_status(&self) -> Option<Self::ExitStatus>;

    fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<Self::ExitStatus> {
        let mut error = None;
        loop {
            if let Some(exit_status) = self.exit_status() {
                return Ok(exit_status);
            }
            terminal.draw(|f| {
                if let Err(e) = self.render(f) {
                    error = Some(e);
                }
                if let Some(e) = error.as_ref() {
                    render_error(f, e);
                }
            })?;
            if let Event::Key(key) = crossterm::event::read()? {
                // When we're displaying an error, any key will dismiss the error popup. They key
                // should then be ignored.
                if error.take().is_some() {
                    continue;
                }
                if let Err(e) = self.handle_key(key) {
                    error = Some(e);
                }
            }
        }
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

fn split_vertical(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    (chunks[0], chunks[1])
}

fn render_error(f: &mut Frame<CrosstermBackend<Stdout>>, error: &anyhow::Error) {
    let area = message_area(f.size());
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

fn message_area(area: Rect) -> Rect {
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Percentage(25),
            Constraint::Percentage(50),
            Constraint::Percentage(25),
        ])
        .split(area);

    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![
            Constraint::Percentage(10),
            Constraint::Percentage(80),
            Constraint::Percentage(10),
        ])
        .split(vertical_chunks[1]);
    horizontal_chunks[1]
}

fn render_list(
    f: &mut Frame<CrosstermBackend<Stdout>>,
    title: &str,
    items: impl Iterator<Item = ListItem<'static>>,
    active: bool,
    area: Rect,
    index: usize,
) {
    let items: Vec<_> = items.collect();
    let mut block = Block::default().title(title).borders(Borders::ALL);
    if active {
        block = block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Yellow));
    }
    let mut style = Style::default().add_modifier(Modifier::REVERSED);
    if active {
        style = style.fg(Color::Yellow);
    }
    let list = List::new(items).block(block).highlight_style(style);
    let mut list_state = ListState::default();
    list_state.select(Some(index));
    f.render_stateful_widget(list, area, &mut list_state);
}

/// Increment or decrement `counter`, wrapping at `len`. `keycode` must be Down or Up.
fn update_counter(counter: &mut usize, key_code: KeyCode, len: usize) {
    match key_code {
        KeyCode::Up => *counter = (*counter + len - 1) % len,
        KeyCode::Down => *counter = (*counter + len + 1) % len,
        _ => panic!("Invalid call to update_counter"),
    }
}

/// A screen that just displays an error with nothing behind it.
struct ErrorScreen<'a> {
    error: &'a anyhow::Error,
    exit_status: Option<()>,
}

impl<'a> ErrorScreen<'a> {
    fn new(error: &'a anyhow::Error) -> Self {
        Self {
            error,
            exit_status: None,
        }
    }
}

impl<'a> Screen for ErrorScreen<'a> {
    type ExitStatus = ();

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        render_error(f, self.error);
        Ok(())
    }

    fn handle_key(&mut self, _key: KeyEvent) -> Result<()> {
        self.exit_status = Some(());
        Ok(())
    }

    fn exit_status(&self) -> Option<Self::ExitStatus> {
        self.exit_status
    }
}

/// A screen that just displays a message.
struct MessageScreen {
    title: String,
    message: String,
    exit_status: Option<()>,
}

impl MessageScreen {
    fn new(title: &str, message: &str) -> Self {
        Self {
            title: title.to_owned(),
            message: message.to_owned(),
            exit_status: None,
        }
    }
}

impl Screen for MessageScreen {
    type ExitStatus = ();

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        let area = message_area(f.size());
        let block = Block::default()
            .title(self.title.as_str())
            .borders(Borders::ALL);
        let paragraph = Paragraph::new(self.message.clone())
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(Clear, area);
        f.render_widget(paragraph, area);
        Ok(())
    }

    fn handle_key(&mut self, _key: KeyEvent) -> Result<()> {
        self.exit_status = Some(());
        Ok(())
    }

    fn exit_status(&self) -> Option<Self::ExitStatus> {
        self.exit_status
    }
}
