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

/// Best-effort return of the terminal to a sane state: leave raw mode,
/// leave the alternate screen, show the cursor. Idempotent, so the RAII
/// guard's `Drop`, the panic hook, and the SIGINT handler can all call it.
fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        terminal::LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
}

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
        restore_terminal();
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
///
/// `_rt` is the process tokio runtime. In live mode the ingest loop it drives
/// was already spawned by `init_live_mode`; the TUI itself is synchronous and
/// reads the shared TSDB. The handle is kept in the signature so the runtime
/// outlives the ingest task and for future async-driven refresh.
pub fn run_tui(state: AppState, live: bool, _rt: &tokio::runtime::Runtime) {
    // Populate the nav (file mode: from the loaded data; live mode: the
    // ingest loop is already running and has produced at least the initial
    // context, but regenerate to pick up all metrics seen so far).
    metadata::regenerate_dashboards(&state);

    let mut app = App::new(initial_sections(&state));
    let overview_tiles = render::overview::tiles();

    // A panic inside the draw loop, or an external SIGINT (`kill -INT`, a
    // supervisor) delivered while the terminal is in raw mode, would
    // otherwise leave the terminal unusable — neither runs `TerminalGuard`'s
    // `Drop`. Restore the terminal in both paths. (A keyboard Ctrl-C in raw
    // mode arrives as a key event and quits gracefully via the guard; the
    // process-wide exit-on-SIGINT handler is intentionally not installed for
    // `--tui`, see `run()`.)
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic(info);
    }));
    let _ = ctrlc::set_handler(|| {
        restore_terminal();
        std::process::exit(130);
    });

    let mut guard = match TerminalGuard::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("failed to initialize terminal: {e}");
            return;
        }
    };

    // Redraw cadence: 1s, or coarser if the source samples slower.
    let tick = Duration::from_millis(1000);

    // Whether the visible screen needs re-querying + redrawing this
    // iteration. Live mode is always dirty (new samples arrive each tick);
    // file mode is dirty only after input/resize (the data never changes),
    // so an idle file-mode TUI does no work between keystrokes instead of
    // re-running PromQL for every visible plot every second.
    let mut dirty = true;

    loop {
        // In live mode, refresh the nav so newly-seen sections appear (and
        // vanished ones are pruned), preserving loaded bodies and selection.
        if live {
            metadata::regenerate_dashboards(&state);
            app.reconcile_sections(initial_sections(&state));
            dirty = true;
        }

        let selected = app.selected_section;
        ensure_section_loaded(&state, &mut app, selected);

        if dirty {
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
            dirty = false;
        }

        // Input with a timeout so live mode keeps ticking. Any key or resize
        // marks the screen dirty so the next iteration re-queries and redraws.
        let poll = event::poll(tick).unwrap_or(false);
        if poll {
            match event::read() {
                Ok(Event::Key(k)) if k.kind == event::KeyEventKind::Press => {
                    if let Some(key) = decode_key(k) {
                        dirty = true;
                        match app.on_key(key) {
                            Action::Quit => break,
                            Action::LoadSection(idx) => {
                                ensure_section_loaded(&state, &mut app, idx);
                            }
                            Action::None | Action::Redraw => {}
                        }
                    }
                }
                Ok(Event::Resize(_, _)) => dirty = true,
                _ => {}
            }
        }
    }
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn ctrl_c_maps_to_quit_before_bare_c() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(decode_key(ctrl_c), Some(Key::Quit));
        // A bare 'c' is not a mapped key.
        assert_eq!(decode_key(press(KeyCode::Char('c'))), None);
    }

    #[test]
    fn keybindings_decode_as_documented() {
        assert_eq!(decode_key(press(KeyCode::Char('q'))), Some(Key::Quit));
        assert_eq!(decode_key(press(KeyCode::Char('?'))), Some(Key::Help));
        assert_eq!(decode_key(press(KeyCode::Tab)), Some(Key::ToggleScreen));
        assert_eq!(
            decode_key(press(KeyCode::Char('o'))),
            Some(Key::ToggleScreen)
        );
        assert_eq!(decode_key(press(KeyCode::Char(']'))), Some(Key::GrowWindow));
        assert_eq!(
            decode_key(press(KeyCode::Char('['))),
            Some(Key::ShrinkWindow)
        );
        assert_eq!(decode_key(press(KeyCode::Down)), Some(Key::Down));
        assert_eq!(decode_key(press(KeyCode::Char('j'))), Some(Key::Down));
        assert_eq!(decode_key(press(KeyCode::Up)), Some(Key::Up));
        assert_eq!(decode_key(press(KeyCode::Char('k'))), Some(Key::Up));
        assert_eq!(decode_key(press(KeyCode::Enter)), Some(Key::Descend));
        assert_eq!(decode_key(press(KeyCode::Right)), Some(Key::Descend));
        assert_eq!(decode_key(press(KeyCode::Char('l'))), Some(Key::Descend));
        assert_eq!(decode_key(press(KeyCode::Esc)), Some(Key::Ascend));
        assert_eq!(decode_key(press(KeyCode::Left)), Some(Key::Ascend));
        assert_eq!(decode_key(press(KeyCode::Char('h'))), Some(Key::Ascend));
        assert_eq!(decode_key(press(KeyCode::Char('r'))), Some(Key::Refresh));
    }

    #[test]
    fn unmapped_key_is_ignored() {
        assert_eq!(decode_key(press(KeyCode::Char('z'))), None);
        assert_eq!(decode_key(press(KeyCode::F(5))), None);
    }
}
