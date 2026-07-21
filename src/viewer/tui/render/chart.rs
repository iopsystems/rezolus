use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols;
use ratatui::text::Span;
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType};
use ratatui::Frame;

use crate::viewer::tui::query::{ChartData, Series};

/// Palette for series lines, cycled by index.
const PALETTE: [Color; 6] = [
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Red,
    Color::Blue,
];

/// Render a single plot into `area`. Handles the Lines / Empty / Error /
/// Unsupported states.
pub fn draw_chart(f: &mut Frame, area: Rect, title: &str, data: &ChartData) {
    let block = Block::default().borders(Borders::ALL).title(title.to_string());
    match data {
        ChartData::Lines(series) => draw_lines(f, area, block, series),
        ChartData::Empty => {
            f.render_widget(block.title(format!("{title} — no data")), area);
        }
        ChartData::Unsupported => {
            f.render_widget(
                block.title(format!("{title} — heatmap (not shown in TUI)")),
                area,
            );
        }
        ChartData::Error(msg) => {
            f.render_widget(block.title(format!("{title} — query failed: {msg}")), area);
        }
    }
}

fn bounds(series: &[Series]) -> ([f64; 2], [f64; 2]) {
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    let mut ymin = f64::INFINITY;
    let mut ymax = f64::NEG_INFINITY;
    for s in series {
        for &(x, y) in &s.points {
            xmin = xmin.min(x);
            xmax = xmax.max(x);
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    if !xmin.is_finite() {
        xmin = 0.0;
        xmax = 1.0;
    }
    if !ymin.is_finite() {
        ymin = 0.0;
        ymax = 1.0;
    }
    if (ymax - ymin).abs() < f64::EPSILON {
        ymax = ymin + 1.0;
    }
    // A zero-width x range makes ratatui's Canvas drop every point (it
    // divides by the x span), so a single-point series — or the first
    // live poll where all points share one timestamp — would render
    // blank. Widen it the same way we widen a flat y range.
    if (xmax - xmin).abs() < f64::EPSILON {
        xmax = xmin + 1.0;
    }
    ([xmin, xmax], [ymin, ymax])
}

fn draw_lines(f: &mut Frame, area: Rect, block: Block, series: &[Series]) {
    let (xb, yb) = bounds(series);
    let datasets: Vec<Dataset> = series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Dataset::default()
                .name(s.label.clone())
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(PALETTE[i % PALETTE.len()]))
                .data(&s.points)
        })
        .collect();

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().bounds(xb))
        .y_axis(
            Axis::default().bounds(yb).labels(vec![
                Span::raw(format!("{:.3}", yb[0])),
                Span::raw(format!("{:.3}", yb[1])),
            ]),
        );
    f.render_widget(chart, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(data: &ChartData, w: u16, h: u16) {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            draw_chart(f, f.area(), "Test", data);
        })
        .unwrap();
    }

    /// Render and return the flattened buffer text, so tests can assert
    /// that something was actually painted (not just "did not panic").
    fn render_text(data: &ChartData, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            draw_chart(f, f.area(), "Test", data);
        })
        .unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_lines_without_panic() {
        let data = ChartData::Lines(vec![Series {
            label: "p50".into(),
            points: vec![(0.0, 1.0), (1.0, 2.0), (2.0, 1.5)],
        }]);
        render(&data, 60, 20);
    }

    #[test]
    fn renders_empty_and_error_and_unsupported() {
        render(&ChartData::Empty, 60, 20);
        render(&ChartData::Error("boom".into()), 60, 20);
        render(&ChartData::Unsupported, 60, 20);
    }

    #[test]
    fn renders_tiny_area_without_panic() {
        let data = ChartData::Lines(vec![Series {
            label: "x".into(),
            points: vec![(0.0, 0.0)],
        }]);
        render(&data, 12, 5);
    }

    #[test]
    fn single_point_series_paints_a_glyph() {
        // A degenerate x range (one point, or all points at one timestamp)
        // must still draw the point — regression guard for the missing
        // x-bounds widening in `bounds()`.
        let data = ChartData::Lines(vec![Series {
            label: "p50".into(),
            points: vec![(5.0, 5.0)],
        }]);
        let text = render_text(&data, 40, 15);
        // Braille plot glyphs live in the U+2800 block; at least one must
        // be present in the rendered buffer.
        assert!(
            text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "expected a braille plot glyph, buffer was: {text:?}"
        );
    }
}
