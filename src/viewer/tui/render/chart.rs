use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols;
use ratatui::text::Span;
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType};
use ratatui::Frame;

use crate::viewer::tui::query::{min_max_decimate, ChartData, Series};
use crate::viewer::tui::units::format_value;

/// Series palette, cycled by index. Explicit high-luminance RGB (not the
/// mid-tone ANSI names) so lines pop against a dark terminal background, and
/// ordered as a cool->hot ramp so percentile series (p50, p90, p99, p999,
/// p9999 — added in that order) also read semantically: cool = low, hot = high.
const PALETTE: [Color; 6] = [
    Color::Rgb(120, 220, 255), // cyan  (p50)
    Color::Rgb(120, 240, 150), // green (p90)
    Color::Rgb(255, 215, 90),  // amber (p99)
    Color::Rgb(255, 140, 90),  // orange (p999)
    Color::Rgb(255, 105, 150), // hot pink/red (p9999)
    Color::Rgb(160, 175, 255), // periwinkle (extra series)
];

/// Render a single plot into `area`. Handles the Lines / Empty / Error /
/// Unsupported states. `unit` is the plot's unit system (e.g. `"percentage"`,
/// `"time"`, `"bytes"`) used to format the y-axis value labels.
pub fn draw_chart(f: &mut Frame, area: Rect, title: &str, unit: Option<&str>, data: &ChartData) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    match data {
        ChartData::Lines(series) => draw_lines(f, area, block, unit, series),
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
            // A +Inf quantile (one past a histogram's highest finite bucket)
            // would make the y range infinite; `draw_lines` does not draw it.
            if y.is_finite() {
                ymin = ymin.min(y);
                ymax = ymax.max(y);
            }
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

/// The runs of consecutive finite points in `points`. A non-finite value (a
/// +Inf quantile, one past a histogram's highest finite bucket) ends a run,
/// so it is drawn as a gap rather than a line joining its neighbours.
fn finite_runs(points: &[(f64, f64)]) -> Vec<Vec<(f64, f64)>> {
    points
        .split(|p| !p.1.is_finite())
        .filter(|run| !run.is_empty())
        .map(<[(f64, f64)]>::to_vec)
        .collect()
}

fn draw_lines(f: &mut Frame, area: Rect, block: Block, unit: Option<&str>, series: &[Series]) {
    let (xb, yb) = bounds(series);
    // Downsample each series to about one bucket per horizontal cell (braille
    // gives ~2 sub-columns/cell), keeping per-bucket min/max so spikes survive
    // and ratatui iterates O(width) points per frame instead of the full,
    // possibly thousands-long, series. `runs` outlives the `Chart` below.
    let target = (area.width as usize).saturating_mul(2).max(1);
    // Each series is drawn as one dataset per run of finite points
    // (`finite_runs`), so a non-finite value is a gap; `bounds` leaves it out
    // of the y range. Each run gets its share of the cell budget. Only a
    // series' first run is named, so the legend lists the series once
    // (ratatui leaves an unnamed dataset out of it).
    let runs: Vec<(usize, Vec<(f64, f64)>)> = series
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            let total = s.points.len().max(1);
            finite_runs(&s.points).into_iter().map(move |run| {
                let share = (target * run.len()).div_ceil(total).max(1);
                (i, min_max_decimate(&run, share))
            })
        })
        .collect();
    let mut named = vec![false; series.len()];
    let datasets: Vec<Dataset> = runs
        .iter()
        .map(|(i, run)| {
            let dataset = Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(PALETTE[i % PALETTE.len()]))
                .data(run);
            if named[*i] {
                dataset
            } else {
                named[*i] = true;
                dataset.name(series[*i].label.clone())
            }
        })
        .collect();

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().bounds(xb))
        .y_axis(Axis::default().bounds(yb).labels(vec![
            Span::raw(format_value(unit, yb[0])),
            Span::raw(format_value(unit, yb[1])),
        ]));
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
            draw_chart(f, f.area(), "Test", Some("percentage"), data);
        })
        .unwrap();
    }

    /// Render and return the flattened buffer text, so tests can assert
    /// that something was actually painted (not just "did not panic").
    fn render_text(data: &ChartData, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            draw_chart(f, f.area(), "Test", Some("percentage"), data);
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
    fn a_series_splits_into_runs_at_non_finite_values() {
        let pts = [
            (0.0, 1.0),
            (1.0, 2.0),
            (2.0, f64::INFINITY),
            (3.0, 2.0),
            (4.0, f64::NAN),
            (5.0, f64::NEG_INFINITY),
            (6.0, 1.0),
        ];
        assert_eq!(
            finite_runs(&pts),
            vec![
                vec![(0.0, 1.0), (1.0, 2.0)],
                vec![(3.0, 2.0)],
                vec![(6.0, 1.0)]
            ]
        );
        assert!(finite_runs(&[(0.0, f64::INFINITY)]).is_empty());
    }

    /// A +Inf quantile is a gap: the line stops before it and starts again
    /// after it, rather than joining its neighbours.
    #[test]
    fn an_infinite_value_is_a_gap_in_the_line() {
        let series = |points: Vec<(f64, f64)>| {
            vec![Series {
                label: "p99".into(),
                points,
            }]
        };
        let with = series(vec![
            (0.0, 1.0),
            (1.0, 3.0),
            (2.0, f64::INFINITY),
            (3.0, 1.0),
            (4.0, 3.0),
        ]);
        assert_eq!(bounds(&with).1, [1.0, 3.0]);
        let gap = render_text(&ChartData::Lines(with), 40, 15);
        let joined = render_text(
            &ChartData::Lines(series(vec![(0.0, 1.0), (1.0, 3.0), (3.0, 1.0), (4.0, 3.0)])),
            40,
            15,
        );
        assert!(
            gap.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)),
            "the finite runs are drawn: {gap:?}"
        );
        assert_ne!(gap, joined, "the line is not joined across the +Inf point");
        // The legend names the series once.
        assert_eq!(gap.matches("p99").count(), 1, "{gap:?}");
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

    #[test]
    fn long_series_renders_via_decimation_without_panic() {
        // Exercise the min/max decimation path in draw_lines with far more
        // points than horizontal cells, including a buried spike.
        let mut points: Vec<(f64, f64)> = (0..5000).map(|i| (i as f64, 1.0)).collect();
        points[2500].1 = 900.0;
        let data = ChartData::Lines(vec![Series {
            label: "p999".into(),
            points,
        }]);
        let text = render_text(&data, 60, 18);
        assert!(text.chars().any(|c| ('\u{2801}'..='\u{28FF}').contains(&c)));
    }
}
