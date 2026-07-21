//! The centered help overlay, toggled with `?`.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

const HELP: &str = "\
Tab / o   toggle Overview <-> Browser
j / k     move selection / scroll
Enter / l descend (nav -> charts)
Esc / h   back (charts -> nav)
[ / ]     shrink / grow time window
r         refresh
?         toggle this help
q         quit";

/// Render a centered help overlay on top of the current screen.
pub fn draw_help(f: &mut Frame) {
    let area = centered(50, 12, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(HELP).block(Block::default().borders(Borders::ALL).title("Help")),
        area,
    );
}

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(h)) / 2),
            Constraint::Length(h),
            Constraint::Min(0),
        ])
        .split(area);
    let hh = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(w)) / 2),
            Constraint::Length(w),
            Constraint::Min(0),
        ])
        .split(v[1]);
    hh[1]
}
