//! The section browser screen: a nav list of sections on the left and a
//! stacked-charts pane on the right (or single-pane on narrow terminals).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::chart::draw_chart;
use crate::viewer::tui::app::{App, BrowserFocus};
use crate::viewer::tui::layout::{browser_split, too_small, BrowserSplit};
use crate::viewer::tui::query::ChartData;

/// Render the section browser. `plots` are the currently selected section's
/// resolved plots (title + loaded data), already produced by the loop.
pub fn draw_browser(f: &mut Frame, app: &App, plots: &[(String, ChartData)]) {
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

fn draw_charts(f: &mut Frame, area: Rect, app: &App, plots: &[(String, ChartData)]) {
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

    // Stack plots vertically at a fixed per-chart height; apply scroll.
    const CHART_H: u16 = 10;
    let visible = (inner.height / CHART_H).max(1) as usize;
    let start = (app.chart_scroll as usize).min(plots.len().saturating_sub(1));
    let constraints: Vec<Constraint> = vec![Constraint::Length(CHART_H); visible];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    for (slot, (title, data)) in rows.iter().zip(plots.iter().skip(start)) {
        draw_chart(f, *slot, title, data);
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

    fn plots() -> Vec<(String, ChartData)> {
        vec![(
            "Usage".into(),
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
}
