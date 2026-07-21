//! Terminal UI frontend for `rezolus view --tui`.

use super::state::AppState;

mod window;

/// Entry point for the TUI. Replaces the axum server when `--tui` is set.
pub fn run_tui(_state: AppState, _live: bool, _rt: &tokio::runtime::Runtime) {
    eprintln!("TUI not yet implemented");
}
