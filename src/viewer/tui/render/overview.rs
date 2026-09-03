//! The curated overview screen: a fixed, priority-ordered set of metric
//! tiles laid out in a responsive grid (see `layout::overview_grid`).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use ::dashboard::blockio_device_latency_metric;
use metriken_query::MetricsSource;

use super::chart::draw_chart;
use crate::viewer::tui::layout::{overview_grid, too_small};
use crate::viewer::tui::model::{PlotDef, PlotKind, DEFAULT_PERCENTILES};
use crate::viewer::tui::query::ChartData;

/// A curated overview tile: a display title and the query that feeds it.
pub struct Tile {
    pub title: &'static str,
    pub def: PlotDef,
}

fn line(query: &str, unit: Option<&str>) -> PlotDef {
    PlotDef {
        title: String::new(),
        base_query: query.to_string(),
        kind: PlotKind::Line,
        percentiles: vec![],
        unit_system: unit.map(|s| s.to_string()),
    }
}

fn pct(query: &str) -> PlotDef {
    PlotDef {
        title: String::new(),
        base_query: query.to_string(),
        kind: PlotKind::Percentiles,
        percentiles: DEFAULT_PERCENTILES.to_vec(),
        unit_system: Some("time".into()),
    }
}

/// The v1 curated tile set, in priority order (lowest priority dropped
/// first when the terminal is small). Queries mirror the web overview
/// section (`crates/dashboard/src/dashboard/overview.rs`) and the memory
/// section so they resolve against the real metric catalog.
/// `data` is consulted only to resolve metric-name aliases (currently just the
/// blockio latency either-name pick via `blockio_device_latency_metric`)
/// against the recording actually loaded. The tile list's length and order are
/// otherwise data-independent — callers and tests may rely on both staying
/// fixed; only individual tiles' query text adapts to `data`. Because the
/// resolution happens at call time, a caller whose data can gain metrics later
/// (a live agent still filling in its catalog) must re-call `tiles()` after
/// that data changes, not just once at startup.
pub fn tiles(data: &dyn MetricsSource) -> Vec<Tile> {
    vec![
        // CPU busy fraction: ns of CPU consumed per second / cores / 1e9.
        Tile {
            title: "CPU Utilization",
            def: line(
                "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000",
                Some("percentage"),
            ),
        },
        Tile {
            title: "Runqueue Latency",
            def: pct("scheduler_runqueue_latency"),
        },
        Tile {
            title: "Syscall Rate",
            def: line("sum(irate(syscall[5m]))", Some("rate")),
        },
        Tile {
            title: "Network Throughput",
            def: line("sum(irate(network_bytes[5m])) * 8", Some("bitrate")),
        },
        Tile {
            title: "Block IO Latency",
            def: pct(blockio_device_latency_metric(data)),
        },
        Tile {
            title: "Memory Used",
            def: line("memory_total - memory_available", Some("bytes")),
        },
    ]
}

/// Render the overview grid. `data` is the loaded chart data for each tile,
/// in the same order as `tiles()`. `data` must have at least as many
/// entries as `tiles` (the caller loads one `ChartData` per tile); any
/// tile beyond `data.len()` is simply not visited since `plan.visible`
/// never exceeds `tiles.len()` and callers are expected to size `data`
/// to match `tiles`.
pub fn draw_overview(f: &mut Frame, tiles: &[Tile], data: &[ChartData]) {
    let area = f.area();
    if too_small(area.width, area.height) {
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("terminal too small — resize or press Tab"),
            area,
        );
        return;
    }

    let plan = overview_grid(area.width, area.height, tiles.len());
    if plan.visible == 0 {
        return;
    }

    // Bound visible by both tiles.len() and data.len() so a caller that
    // passes mismatched slices cannot trigger an out-of-bounds index below.
    let visible = plan.visible.min(tiles.len()).min(data.len());
    if visible == 0 {
        return;
    }

    // Fixed-height rows packed from the top; a trailing flexible spacer
    // absorbs leftover height so tiles stay landscape instead of stretching.
    let mut row_constraints: Vec<Constraint> =
        vec![Constraint::Length(plan.row_height); plan.rows as usize];
    row_constraints.push(Constraint::Min(0));
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for r in 0..plan.rows {
        let Some(row_area) = row_areas.get(r as usize) else {
            break;
        };
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![
                Constraint::Ratio(1, plan.cols.max(1) as u32);
                plan.cols as usize
            ])
            .split(*row_area);
        for c in 0..plan.cols {
            let idx = (r * plan.cols + c) as usize;
            if idx >= visible {
                break;
            }
            let Some(cell_area) = cells.get(c as usize) else {
                break;
            };
            draw_tile(f, *cell_area, &tiles[idx], &data[idx]);
        }
    }
}

fn draw_tile(f: &mut Frame, area: Rect, tile: &Tile, data: &ChartData) {
    draw_chart(f, area, tile.title, tile.def.unit_system.as_deref(), data);
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_at(w: u16, h: u16) {
        let store = MemoryStore::builder().build();
        let tiles = tiles(&store);
        let data: Vec<ChartData> = tiles.iter().map(|_| ChartData::Empty).collect();
        assert_eq!(data.len(), tiles.len());
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw_overview(f, &tiles, &data)).unwrap();
    }

    #[test]
    fn renders_full_grid() {
        render_at(120, 40);
    }

    #[test]
    fn renders_reduced_grid_small() {
        render_at(50, 14);
    }

    #[test]
    fn renders_too_small() {
        render_at(20, 8);
    }

    #[test]
    fn tile_count_is_stable() {
        let store = MemoryStore::builder().build();
        assert_eq!(tiles(&store).len(), 6);
    }

    /// The blockio tile follows the recording's metric name. An empty store has
    /// neither name, so it must fall back to the one a current agent emits
    /// rather than the retired pre-rename name.
    #[test]
    fn blockio_tile_uses_the_name_the_recording_has() {
        let store = MemoryStore::builder().build();
        let blockio = tiles(&store)
            .into_iter()
            .find(|t| t.title == "Block IO Latency")
            .expect("blockio tile present");
        assert_eq!(blockio.def.base_query, "blockio_device_latency");
    }
}
