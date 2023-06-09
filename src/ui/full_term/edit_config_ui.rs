//! Terminal user interface for basic editing of the configuration with the exception of fixing
//! problems. Primarily this is used for creating the initial configuration.

use super::render_list;
use super::update_counter;
use super::Screen;
use crate::config::MAX_VERSION;
use crate::config::SANDBOX_KINDS;
use crate::config_editor::ConfigEditor;
use crate::ui::FixOutcome;
use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::ListItem;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use std::collections::HashSet;
use std::io::Stdout;
use std::path::PathBuf;

pub(super) struct EditConfigUi {
    mode: Mode,
    editor: ConfigEditor,
    action_index: usize,
    item_index: usize,
    config_path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    SelectAction,
    RenderingAction,
    Quit,
    Continue,
}

const ACTIONS: &[&dyn Action] = &[&SelectSandbox, &SelectImports, &WriteConfig, &Quit];

impl Screen for EditConfigUi {
    type ExitStatus = FixOutcome;

    fn exit_status(&self) -> Option<Self::ExitStatus> {
        match self.mode {
            Mode::Quit => Some(FixOutcome::GiveUp),
            Mode::Continue => Some(FixOutcome::Retry),
            _ => None,
        }
    }

    fn render(&self, f: &mut Frame<CrosstermBackend<Stdout>>) -> Result<()> {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .margin(1)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(f.size());

        let (top_left, bottom_left) = super::split_vertical(horizontal[0]);

        render_list(
            f,
            "Edit config",
            ACTIONS
                .iter()
                .map(|ui| ListItem::new(ui.title().to_owned())),
            self.mode == Mode::SelectAction,
            top_left,
            self.action_index,
        );
        self.render_action_help(f, bottom_left);
        match self.mode {
            Mode::SelectAction => self.render_config(f, horizontal[1]),
            Mode::RenderingAction => ACTIONS[self.action_index].render(f, horizontal[1], self)?,
            _ => {}
        }

        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match (self.mode, key.code) {
            (_, KeyCode::Char('q')) => self.mode = Mode::Quit,
            (Mode::SelectAction, KeyCode::Up | KeyCode::Down) => {
                update_counter(&mut self.action_index, key.code, ACTIONS.len());
            }
            (Mode::RenderingAction, KeyCode::Up | KeyCode::Down) => {
                update_counter(
                    &mut self.item_index,
                    key.code,
                    ACTIONS[self.action_index].num_items(),
                );
            }
            (Mode::SelectAction, KeyCode::Char(' ') | KeyCode::Enter) => {
                ACTIONS[self.action_index].run(self)?;
                // If running the action didn't change our mode, then switch to rendering the
                // action.
                if self.mode == Mode::SelectAction {
                    self.mode = Mode::RenderingAction;
                }
            }
            (Mode::RenderingAction, KeyCode::Char(' ') | KeyCode::Enter) => {
                let action = ACTIONS[self.action_index];
                if self.item_index < action.num_items() {
                    action.item_selected(self.item_index, self)?;
                }
            }
            (_, KeyCode::Esc) => self.mode = Mode::SelectAction,
            _ => {}
        }
        Ok(())
    }
}

impl EditConfigUi {
    pub(super) fn new(config_path: PathBuf) -> Self {
        let mut editor = ConfigEditor::initial();
        editor.set_version(MAX_VERSION);
        Self {
            mode: Mode::SelectAction,
            editor,
            action_index: 0,
            item_index: 0,
            config_path,
        }
    }

    fn render_config(&self, f: &mut Frame<CrosstermBackend<Stdout>>, area: ratatui::layout::Rect) {
        let block = Block::default().title("Config").borders(Borders::ALL);
        let paragraph = Paragraph::new(self.editor.to_toml())
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }

    fn render_action_help(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: ratatui::layout::Rect,
    ) {
        let block = Block::default().title("Help").borders(Borders::ALL);
        let action = ACTIONS[self.action_index];
        let paragraph = Paragraph::new(action.help())
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
    }
}

trait Action {
    fn title(&self) -> &'static str;

    fn help(&self) -> &'static str;

    fn run(&self, _ui: &mut EditConfigUi) -> Result<()> {
        Ok(())
    }

    /// Returns the number of items that this action is displaying in a list (if any).
    fn num_items(&self) -> usize {
        0
    }

    fn render(
        &self,
        _f: &mut Frame<CrosstermBackend<Stdout>>,
        _area: ratatui::layout::Rect,
        _ui: &EditConfigUi,
    ) -> Result<()> {
        Ok(())
    }

    fn item_selected(&self, _index: usize, _ui: &mut EditConfigUi) -> Result<()> {
        Ok(())
    }
}

struct SelectSandbox;

impl Action for SelectSandbox {
    fn title(&self) -> &'static str {
        "Select sandbox kind"
    }

    fn help(&self) -> &'static str {
        "Select what kind of sandbox to use when running build scripts - and maybe some day proc \
         macros. If using Bubblewrap, it must be installed. On Debian-based systems you can run\n\
         `sudo apt install bubblewrap`."
    }

    fn render(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: ratatui::layout::Rect,
        ui: &EditConfigUi,
    ) -> Result<()> {
        render_list(
            f,
            "Select sandbox",
            SANDBOX_KINDS
                .iter()
                .map(|kind| ListItem::new(format!("{:?}", kind))),
            true,
            area,
            ui.item_index,
        );
        Ok(())
    }

    fn num_items(&self) -> usize {
        SANDBOX_KINDS.len()
    }

    fn item_selected(&self, index: usize, ui: &mut EditConfigUi) -> Result<()> {
        ui.editor.set_sandbox_kind(SANDBOX_KINDS[index])?;
        ui.mode = Mode::SelectAction;
        Ok(())
    }
}

struct SelectImports;

impl Action for SelectImports {
    fn title(&self) -> &'static str {
        "Select std API imports"
    }

    fn help(&self) -> &'static str {
        "Pick which APIs from the Rust standard library you'd like to restrict access to."
    }

    fn render(
        &self,
        f: &mut Frame<CrosstermBackend<Stdout>>,
        area: ratatui::layout::Rect,
        ui: &EditConfigUi,
    ) -> Result<()> {
        let built_ins = crate::config::built_in::get_built_ins();
        let enabled: HashSet<_> = ui
            .editor
            .std_imports()
            .map(|imp| imp.collect())
            .unwrap_or_default();
        render_list(
            f,
            "Select sandbox",
            built_ins.keys().map(|k| {
                let name = k.to_string();
                let is_enabled = enabled.contains(name.as_str());
                let mut item = ListItem::new(name);
                if is_enabled {
                    item = item.style(Style::default().add_modifier(Modifier::BOLD));
                }
                item
            }),
            true,
            area,
            ui.item_index,
        );
        Ok(())
    }

    fn num_items(&self) -> usize {
        crate::config::built_in::get_built_ins().len()
    }

    fn item_selected(&self, index: usize, ui: &mut EditConfigUi) -> Result<()> {
        let built_ins = crate::config::built_in::get_built_ins();
        let item_name = built_ins
            .keys()
            .nth(index)
            .ok_or_else(|| anyhow!("Invalid index"))?
            .to_string();
        ui.editor.toggle_std_import(&item_name)?;
        Ok(())
    }
}

struct WriteConfig;

impl Action for WriteConfig {
    fn title(&self) -> &'static str {
        "Write config and continue"
    }

    fn help(&self) -> &'static str {
        "Write the configuration file and proceed to check it."
    }

    fn run(&self, ui: &mut EditConfigUi) -> Result<()> {
        std::fs::write(&ui.config_path, ui.editor.to_toml())
            .with_context(|| format!("Failed to write `{}`", ui.config_path.display()))?;
        ui.mode = Mode::Continue;
        Ok(())
    }
}

struct Quit;

impl Action for Quit {
    fn title(&self) -> &'static str {
        "Quit"
    }

    fn help(&self) -> &'static str {
        "Exit without writing configuration file."
    }

    fn run(&self, ui: &mut EditConfigUi) -> Result<()> {
        ui.mode = Mode::Quit;
        Ok(())
    }
}
