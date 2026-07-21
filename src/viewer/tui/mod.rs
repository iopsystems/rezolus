//! Terminal UI frontend for `rezolus view --tui`.

mod app;
mod layout;
mod model;
mod query;
mod render;
mod window;

use std::io::{self, Stdout};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::{execute, terminal};
use ratatui::Terminal;

use super::metadata;
use super::state::AppState;
use app::{Action, App, Key, Screen};
use model::{parse_section_groups, NavSection};
use query::{load_chart, ChartData};

/// RAII guard: enters the alternate screen + raw mode on construction and
/// restores the terminal on drop (including on panic/unwind).
struct TerminalGuard {
    term: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, terminal::EnterAlternateScreen)?;
        let term = Terminal::new(CrosstermBackend::new(out))?;
        Ok(Self { term })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(self.term.backend_mut(), terminal::LeaveAlternateScreen);
        let _ = self.term.show_cursor();
    }
}

/// Decode a crossterm key event into our logical `Key`. Returns `None`
/// for keys we ignore.
fn decode_key(k: KeyEvent) -> Option<Key> {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
        return Some(Key::Quit);
    }
    match k.code {
        KeyCode::Char('q') => Some(Key::Quit),
        KeyCode::Char('?') => Some(Key::Help),
        KeyCode::Tab | KeyCode::Char('o') => Some(Key::ToggleScreen),
        KeyCode::Char(']') => Some(Key::GrowWindow),
        KeyCode::Char('[') => Some(Key::ShrinkWindow),
        KeyCode::Down | KeyCode::Char('j') => Some(Key::Down),
        KeyCode::Up | KeyCode::Char('k') => Some(Key::Up),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Some(Key::Descend),
        KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => Some(Key::Ascend),
        KeyCode::Char('r') => Some(Key::Refresh),
        _ => None,
    }
}

/// Build the initial nav section list from the AppState.
fn initial_sections(state: &AppState) -> Vec<NavSection> {
    state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| NavSection {
            name: s.name.clone(),
            route: s.route.clone(),
            groups: None,
        })
        .collect()
}

/// Ensure section `idx`'s groups are loaded into the App.
fn ensure_section_loaded(state: &AppState, app: &mut App, idx: usize) {
    if app
        .sections
        .get(idx)
        .map(|s| s.groups.is_some())
        .unwrap_or(true)
    {
        return;
    }
    let route = app.sections[idx].route.clone();
    let data = state.baseline_data();
    let groups = {
        let mut store = state.sections.write();
        store
            .get_or_generate(&route, data.as_ref())
            .map(parse_section_groups)
    };
    if let Some(groups) = groups {
        app.set_section_groups(idx, groups);
    }
}

/// Load chart data for every plot in the selected section.
fn load_section_charts(state: &AppState, app: &App) -> Vec<(String, ChartData)> {
    let data = state.baseline_data();
    let Some(section) = app.current_section() else {
        return Vec::new();
    };
    let Some(groups) = section.groups.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for g in groups {
        for p in &g.plots {
            let title = if p.title.is_empty() {
                g.name.clone()
            } else {
                p.title.clone()
            };
            out.push((title, load_chart(data.as_ref(), p, app.window)));
        }
    }
    out
}

/// Entry point for the TUI. Replaces the axum server when `--tui` is set.
pub fn run_tui(state: AppState, live: bool, _rt: &tokio::runtime::Runtime) {
    // Populate the nav (file mode: from the loaded data; live mode: the
    // ingest loop is already running and has produced at least the initial
    // context, but regenerate to pick up all metrics seen so far).
    metadata::regenerate_dashboards(&state);

    let mut app = App::new(initial_sections(&state));
    let overview_tiles = render::overview::tiles();

    let mut guard = match TerminalGuard::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("failed to initialize terminal: {e}");
            return;
        }
    };

    // Redraw cadence: 1s, or coarser if the source samples slower.
    let tick = Duration::from_millis(1000);

    loop {
        // In live mode, refresh the nav so newly-seen sections appear.
        if live {
            metadata::regenerate_dashboards(&state);
            // Reconcile: if new sections appeared, extend the nav list.
            let latest = initial_sections(&state);
            if latest.len() != app.sections.len() {
                // Preserve loaded bodies by name where possible.
                for s in latest {
                    if !app.sections.iter().any(|e| e.route == s.route) {
                        app.sections.push(s);
                    }
                }
            }
        }

        let selected = app.selected_section;
        ensure_section_loaded(&state, &mut app, selected);

        // Gather per-screen render inputs.
        let section_charts = if app.screen == Screen::Browser {
            load_section_charts(&state, &app)
        } else {
            Vec::new()
        };
        let overview_data: Vec<ChartData> = if app.screen == Screen::Overview {
            let data = state.baseline_data();
            overview_tiles
                .iter()
                .map(|t| load_chart(data.as_ref(), &t.def, app.window))
                .collect()
        } else {
            Vec::new()
        };

        let draw_res = guard.term.draw(|f| {
            match app.screen {
                Screen::Overview => {
                    render::overview::draw_overview(f, &overview_tiles, &overview_data)
                }
                Screen::Browser => render::browser::draw_browser(f, &app, &section_charts),
            }
            if app.show_help {
                render::help::draw_help(f);
            }
        });
        if draw_res.is_err() {
            break;
        }

        // Input with a timeout so live mode keeps ticking.
        let poll = event::poll(tick).unwrap_or(false);
        if poll {
            match event::read() {
                Ok(Event::Key(k)) if k.kind == event::KeyEventKind::Press => {
                    if let Some(key) = decode_key(k) {
                        match app.on_key(key) {
                            Action::Quit => break,
                            Action::LoadSection(idx) => {
                                ensure_section_loaded(&state, &mut app, idx);
                            }
                            Action::None | Action::Redraw | Action::ToggleHelp => {}
                        }
                    }
                }
                Ok(Event::Resize(_, _)) => { /* loop redraws */ }
                _ => {}
            }
        }
        // In file mode with no pending input, we still loop and redraw;
        // that is cheap and keeps resize handling simple.
    }
    drop(guard);
}
