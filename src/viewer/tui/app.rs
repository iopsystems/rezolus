//! TUI application state machine: current screen, selection, focus, and
//! keyboard handling. Pure — no ratatui, no I/O — so it is unit-testable.

use super::model::NavSection;
use super::window::TimeWindow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Browser,
}

/// Which browser pane has focus (matters only when narrow / single-pane).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserFocus {
    Nav,
    Charts,
}

/// An action the render/event loop must carry out after handling a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Redraw,
    /// The selected section changed; loop should ensure its body is loaded.
    LoadSection(usize),
    ToggleHelp,
}

pub struct App {
    pub screen: Screen,
    pub sections: Vec<NavSection>,
    pub selected_section: usize,
    pub selected_group: usize,
    pub focus: BrowserFocus,
    pub window: TimeWindow,
    pub show_help: bool,
    /// Vertical scroll offset (in rows) of the charts pane.
    pub chart_scroll: u16,
}

impl App {
    pub fn new(sections: Vec<NavSection>) -> Self {
        Self {
            screen: Screen::Overview,
            sections,
            selected_section: 0,
            selected_group: 0,
            focus: BrowserFocus::Nav,
            window: TimeWindow::Last5m,
            show_help: false,
            chart_scroll: 0,
        }
    }

    pub fn current_section(&self) -> Option<&NavSection> {
        self.sections.get(self.selected_section)
    }

    fn section_count(&self) -> usize {
        self.sections.len()
    }

    fn group_count(&self) -> usize {
        self.current_section()
            .and_then(|s| s.groups.as_ref())
            .map(|g| g.len())
            .unwrap_or(0)
    }

    /// Handle a single logical key. Returns the action for the loop.
    pub fn on_key(&mut self, key: Key) -> Action {
        if self.show_help {
            // Any key closes help.
            self.show_help = false;
            return Action::Redraw;
        }
        match key {
            Key::Quit => Action::Quit,
            Key::Help => {
                self.show_help = true;
                Action::Redraw
            }
            Key::ToggleScreen => {
                self.screen = match self.screen {
                    Screen::Overview => Screen::Browser,
                    Screen::Browser => Screen::Overview,
                };
                if self.screen == Screen::Browser {
                    Action::LoadSection(self.selected_section)
                } else {
                    Action::Redraw
                }
            }
            Key::GrowWindow => {
                self.window = self.window.grow();
                Action::Redraw
            }
            Key::ShrinkWindow => {
                self.window = self.window.shrink();
                Action::Redraw
            }
            Key::Down => self.move_down(),
            Key::Up => self.move_up(),
            Key::Descend => self.descend(),
            Key::Ascend => self.ascend(),
            Key::Refresh => Action::LoadSection(self.selected_section),
        }
    }

    fn move_down(&mut self) -> Action {
        match self.screen {
            Screen::Browser if self.focus == BrowserFocus::Nav => {
                if self.section_count() > 0 {
                    self.selected_section =
                        (self.selected_section + 1).min(self.section_count() - 1);
                    self.selected_group = 0;
                    self.chart_scroll = 0;
                    return Action::LoadSection(self.selected_section);
                }
                Action::None
            }
            Screen::Browser => {
                self.chart_scroll = self.chart_scroll.saturating_add(1);
                Action::Redraw
            }
            Screen::Overview => Action::None,
        }
    }

    fn move_up(&mut self) -> Action {
        match self.screen {
            Screen::Browser if self.focus == BrowserFocus::Nav => {
                self.selected_section = self.selected_section.saturating_sub(1);
                self.selected_group = 0;
                self.chart_scroll = 0;
                Action::LoadSection(self.selected_section)
            }
            Screen::Browser => {
                self.chart_scroll = self.chart_scroll.saturating_sub(1);
                Action::Redraw
            }
            Screen::Overview => Action::None,
        }
    }

    fn descend(&mut self) -> Action {
        if self.screen == Screen::Browser && self.focus == BrowserFocus::Nav {
            self.focus = BrowserFocus::Charts;
            return Action::Redraw;
        }
        Action::None
    }

    fn ascend(&mut self) -> Action {
        if self.screen == Screen::Browser && self.focus == BrowserFocus::Charts {
            self.focus = BrowserFocus::Nav;
            return Action::Redraw;
        }
        Action::None
    }

    /// Called by the loop after it has parsed a section's groups.
    pub fn set_section_groups(&mut self, idx: usize, groups: Vec<super::model::NavGroup>) {
        if let Some(s) = self.sections.get_mut(idx) {
            s.groups = Some(groups);
        }
    }
}

/// Logical key, decoded from crossterm events by the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Quit,
    Help,
    ToggleScreen,
    GrowWindow,
    ShrinkWindow,
    Up,
    Down,
    Descend,
    Ascend,
    Refresh,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::model::{NavGroup, NavSection};

    fn sections() -> Vec<NavSection> {
        vec![
            NavSection { name: "CPU".into(), route: "/cpu".into(), groups: None },
            NavSection { name: "Net".into(), route: "/network".into(), groups: None },
        ]
    }

    #[test]
    fn toggle_screen_enters_browser_and_requests_load() {
        let mut app = App::new(sections());
        assert_eq!(app.screen, Screen::Overview);
        let a = app.on_key(Key::ToggleScreen);
        assert_eq!(app.screen, Screen::Browser);
        assert_eq!(a, Action::LoadSection(0));
    }

    #[test]
    fn nav_down_clamps_and_requests_load() {
        let mut app = App::new(sections());
        app.on_key(Key::ToggleScreen); // -> Browser
        let a = app.on_key(Key::Down);
        assert_eq!(app.selected_section, 1);
        assert_eq!(a, Action::LoadSection(1));
        // clamps at end
        app.on_key(Key::Down);
        assert_eq!(app.selected_section, 1);
    }

    #[test]
    fn descend_moves_focus_to_charts_then_scrolls() {
        let mut app = App::new(sections());
        app.on_key(Key::ToggleScreen);
        app.on_key(Key::Descend);
        assert_eq!(app.focus, BrowserFocus::Charts);
        app.on_key(Key::Down);
        assert_eq!(app.chart_scroll, 1);
    }

    #[test]
    fn window_keys_cycle() {
        let mut app = App::new(sections());
        assert_eq!(app.window, TimeWindow::Last5m);
        app.on_key(Key::ShrinkWindow);
        assert_eq!(app.window, TimeWindow::Last1m);
        app.on_key(Key::GrowWindow);
        assert_eq!(app.window, TimeWindow::Last5m);
    }

    #[test]
    fn help_toggles_and_any_key_closes() {
        let mut app = App::new(sections());
        app.on_key(Key::Help);
        assert!(app.show_help);
        app.on_key(Key::Down);
        assert!(!app.show_help);
    }

    #[test]
    fn quit_key_returns_quit() {
        let mut app = App::new(sections());
        assert_eq!(app.on_key(Key::Quit), Action::Quit);
    }

    #[test]
    fn set_section_groups_stores_body() {
        let mut app = App::new(sections());
        app.set_section_groups(0, vec![NavGroup { name: "g".into(), plots: vec![] }]);
        assert!(app.sections[0].groups.is_some());
    }
}
