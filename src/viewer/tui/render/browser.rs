//! The section browser screen: a nav list of sections on the left and a
//! stacked-charts pane on the right (or single-pane on narrow terminals).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use super::chart::draw_chart;
use crate::viewer::tui::app::{App, BrowserFocus};
use crate::viewer::tui::layout::{browser_split, too_small, BrowserSplit};
use crate::viewer::tui::query::ChartData;

/// Fixed height (in cells) of each stacked chart in the browser's charts pane.
pub const CHART_H: u16 = 10;

/// How many charts fit in a charts pane of the given inner height.
pub fn visible_charts(inner_height: u16) -> usize {
    (inner_height / CHART_H).max(1) as usize
}

/// A resolved plot to render: title, unit system, and loaded data.
pub type LoadedPlot = (String, Option<String>, ChartData);

/// Render the section browser. `plots` are the currently selected section's
/// resolved plots (title + unit + loaded data), already produced by the loop.
pub fn draw_browser(f: &mut Frame, app: &App, plots: &[LoadedPlot]) {
    let area = f.area();
    if too_small(area.width, area.height) {
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("terminal too small — resize"),
            area,
        );
        return;
    }

    match browser_split(area.width) {
        BrowserSplit::Dual(nav_w) => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(nav_w), Constraint::Min(0)])
                .split(area);
            draw_nav(f, cols[0], app);
            draw_charts(f, cols[1], app, plots);
        }
        BrowserSplit::Single => match app.focus {
            BrowserFocus::Nav => draw_nav(f, area, app),
            BrowserFocus::Charts => draw_charts(f, area, app, plots),
        },
    }
}

fn draw_nav(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .sections
        .iter()
        .map(|s| ListItem::new(s.name.clone()))
        .collect();
    let focused = app.focus == BrowserFocus::Nav;
    let mut state = ListState::default();
    state.select(Some(app.selected_section));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(if focused {
            "Sections*"
        } else {
            "Sections"
        }))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_charts(f: &mut Frame, area: Rect, app: &App, plots: &[LoadedPlot]) {
    // Inner area inside a titled border; the title also shows the active
    // time window so the effect of the [ / ] keys is visible.
    let section = app
        .current_section()
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let title = format!("{section}  ({})", app.window.label());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if plots.is_empty() {
        return;
    }

    let visible = visible_charts(inner.height);
    // Clamp so the last page fills the pane (and the scrollbar thumb reaches
    // the bottom) instead of scrolling a single chart into an empty pane.
    let max_start = plots.len().saturating_sub(visible);
    let start = (app.chart_scroll as usize).min(max_start);
    let overflow = plots.len() > visible;

    // Reserve the rightmost column for a scrollbar when there's overflow, so
    // it's obvious more charts exist above/below the visible ones.
    let charts_area = if overflow {
        Rect {
            width: inner.width.saturating_sub(1),
            ..inner
        }
    } else {
        inner
    };

    let constraints: Vec<Constraint> = vec![Constraint::Length(CHART_H); visible];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(charts_area);
    for (slot, (title, unit, data)) in rows.iter().zip(plots.iter().skip(start)) {
        draw_chart(f, *slot, title, unit.as_deref(), data);
    }

    if overflow {
        let mut sb_state = ScrollbarState::new(plots.len())
            .viewport_content_length(visible)
            .position(start);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        f.render_stateful_widget(scrollbar, inner, &mut sb_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::tui::app::{App, Key};
    use crate::viewer::tui::model::{NavGroup, NavSection};
    use crate::viewer::tui::query::{ChartData, Series};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app_with_section() -> App {
        let mut app = App::new(vec![NavSection {
            name: "CPU".into(),
            route: "/cpu".into(),
            groups: Some(vec![NavGroup {
                name: "Usage".into(),
                plots: vec![],
            }]),
        }]);
        app.on_key(Key::ToggleScreen);
        app
    }

    fn plots() -> Vec<LoadedPlot> {
        vec![(
            "Usage".into(),
            Some("percentage".into()),
            ChartData::Lines(vec![Series {
                label: "p50".into(),
                points: vec![(0.0, 1.0), (1.0, 2.0)],
            }]),
        )]
    }

    fn render_at(w: u16, h: u16) {
        let app = app_with_section();
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw_browser(f, &app, &plots())).unwrap();
    }

    #[test]
    fn renders_dual_pane_wide() {
        render_at(120, 40);
    }

    #[test]
    fn renders_single_pane_narrow() {
        render_at(40, 15);
    }

    #[test]
    fn renders_too_small() {
        render_at(20, 8);
    }

    #[test]
    fn nav_shows_selected_section_name() {
        let app = app_with_section();
        let backend = TestBackend::new(120, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw_browser(f, &app, &plots())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("CPU"));
    }

    fn many_plots(n: usize) -> Vec<LoadedPlot> {
        (0..n)
            .map(|i| {
                (
                    format!("plot{i}"),
                    None,
                    ChartData::Lines(vec![Series {
                        label: "x".into(),
                        points: vec![(0.0, 1.0), (1.0, 2.0)],
                    }]),
                )
            })
            .collect()
    }

    fn buffer_text(w: u16, h: u16, plots: &[LoadedPlot], scroll: u16) -> String {
        let mut app = app_with_section();
        app.focus = BrowserFocus::Charts;
        app.chart_scroll = scroll;
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw_browser(f, &app, plots)).unwrap();
        term.backend()
            .buffer()
            .clone()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn scrollbar_appears_only_when_charts_overflow() {
        // At 60x24 the charts pane fits ~2 charts; 10 plots overflow.
        let over = buffer_text(60, 24, &many_plots(10), 0);
        assert!(
            over.chars().any(|c| matches!(c, '█' | '▲' | '▼' | '│')),
            "expected a scrollbar glyph when overflowing"
        );
        // A single plot fits, so no scrollbar chrome is drawn.
        let fits = buffer_text(60, 24, &many_plots(1), 0);
        assert!(!fits.chars().any(|c| matches!(c, '█' | '▲' | '▼')));
    }

    #[test]
    fn scroll_is_clamped_so_last_page_fills() {
        // Scroll far past the end; render must still fill the pane from the
        // last page (start clamped), never panic or blank out.
        let text = buffer_text(60, 24, &many_plots(10), 999);
        // The last plots must be visible (not scrolled into the void).
        assert!(text.contains("plot9"));
    }
}
