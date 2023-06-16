//! A fullscreen terminal user interface.

use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
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
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

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

impl super::UserInterface for FullTermUi {
    fn run(
        &mut self,
        problem_store: ProblemStoreRef,
        event_receiver: Receiver<AppEvent>,
    ) -> Result<()> {
        let mut problems_ui =
            problems_ui::ProblemsUi::new(problem_store.clone(), self.config_path.clone());
        let mut edit_ui =
            edit_config_ui::EditConfigUi::new(problem_store.clone(), self.config_path.clone());
        let mut needs_redraw = true;
        let mut error = None;
        loop {
            let mut screen: &mut dyn Screen = &mut problems_ui;
            if edit_ui.is_active() {
                screen = &mut edit_ui;
            }
            if screen.quit_requested() {
                // When quit has been requested, we abort all problems in the store. New problems
                // may be added afterwards, in which case we'll go around the loop again and abort
                // those problems too. We don't return from this function until we get a shutdown
                // event from the main thread.
                problem_store.lock().abort();
            }
            if needs_redraw {
                self.terminal.draw(|f| {
                    if let Err(e) = screen.render(f) {
                        error = Some(e);
                    }
                    if let Some(e) = error.as_ref() {
                        render_error(f, e);
                    }
                })?;
                needs_redraw = false;
            }
            match event_receiver.try_recv() {
                Ok(AppEvent::ProblemsAdded) => {
                    needs_redraw = true;
                }
                Ok(AppEvent::Shutdown) => {
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {
                    // TODO: Consider spawning a separate thread to read crossterm events, then feed
                    // them into the main event channel. That way we can avoid polling.
                    if crossterm::event::poll(Duration::from_millis(100))? {
                        needs_redraw = true;
                        let Ok(Event::Key(key)) = crossterm::event::read() else {
                            continue;
                        };
                        // When we're displaying an error, any key will dismiss the error popup. The key
                        // should then be ignored.
                        if error.take().is_some() {
                            continue;
                        }
                        if let Err(e) = screen.handle_key(key) {
                            error = Some(e);
                        }
                    }
                }
            }
        }
    }
}

trait Screen {
    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()>;
    fn handle_key(&mut self, key: KeyEvent) -> Result<()>;
    fn quit_requested(&self) -> bool;
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

fn render_build_progress(f: &mut Frame<CrosstermBackend<Stdout>>) {
    let area = message_area(f.size());
    let block = Block::default()
        .title("Building")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new("Build in progress")
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
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
