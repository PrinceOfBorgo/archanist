use crate::config::ArchanistConfig;
use crate::state::ArchanistState;
use crate::tui::render;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::time::Duration;

pub struct App {
    pub selected: usize,
    pub component_names: Vec<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(component_names: Vec<String>) -> Self {
        Self {
            selected: 0,
            component_names,
            should_quit: false,
        }
    }

    pub fn next(&mut self) {
        if self.component_names.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.component_names.len();
    }

    pub fn prev(&mut self) {
        if self.component_names.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.component_names.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn selected_component(&self) -> Option<&str> {
        self.component_names.get(self.selected).map(String::as_str)
    }
}

pub fn run_config_editor(cfg: &ArchanistConfig, state: &ArchanistState) -> Result<()> {
    let mut app = App::new(cfg.component_names());
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app, cfg, state);
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    cfg: &ArchanistConfig,
    state: &ArchanistState,
) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| render::draw(frame, app, cfg, state))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                KeyCode::Char('j') | KeyCode::Down => app.next(),
                KeyCode::Char('k') | KeyCode::Up => app.prev(),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_at_first_component() {
        let app = App::new(vec!["a".into(), "b".into()]);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_component(), Some("a"));
    }

    #[test]
    fn next_wraps_around() {
        let mut app = App::new(vec!["a".into(), "b".into()]);
        app.next();
        assert_eq!(app.selected_component(), Some("b"));
        app.next();
        assert_eq!(app.selected_component(), Some("a"));
    }

    #[test]
    fn prev_wraps_around() {
        let mut app = App::new(vec!["a".into(), "b".into()]);
        app.prev();
        assert_eq!(app.selected_component(), Some("b"));
        app.prev();
        assert_eq!(app.selected_component(), Some("a"));
    }

    #[test]
    fn empty_component_list_stays_none() {
        let mut app = App::new(vec![]);
        assert_eq!(app.selected_component(), None);
        app.next();
        app.prev();
        assert_eq!(app.selected_component(), None);
    }
}
