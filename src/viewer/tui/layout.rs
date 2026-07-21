//! Pure responsive layout math. No ratatui types so it is trivially
//! unit-testable; render code consumes these decisions.

/// Minimum terminal size below which we show a "too small" message.
pub const MIN_WIDTH: u16 = 30;
pub const MIN_HEIGHT: u16 = 10;

/// Per-tile minimum height on the overview grid, below which a tile is too
/// cramped to read.
pub const TILE_MIN_H: u16 = 7;

/// Preferred tile width. We fit as many preferred-width tiles across as the
/// terminal allows, rather than as *many* tiles as possible — otherwise a
/// wide terminal packs the tiles into narrow full-height strips. This also
/// keeps every tile comfortably above the readable minimum width.
pub const TILE_PREF_W: u16 = 48;

/// Cap on tile height. Charts read best wider-than-tall, so we never stretch
/// a tile past this even on a tall terminal; rows pack from the top and the
/// leftover height is left blank rather than inflating tiles into vertical
/// strips.
pub const TILE_MAX_H: u16 = 14;

/// Overview grid decision: column/row counts, how many tiles are shown, and
/// the fixed per-row (tile) height in cells.
#[derive(Debug, PartialEq, Eq)]
pub struct GridPlan {
    pub cols: u16,
    pub rows: u16,
    pub visible: usize,
    /// Fixed height of each tile row, in cells (already capped for aspect).
    pub row_height: u16,
}

/// Compute the overview grid for `n` priority-ordered tiles in a `w`x`h`
/// area. Chooses a column count that keeps tiles readably wide, caps tile
/// height so they stay landscape, and drops lowest-priority tiles that do
/// not fit vertically.
pub fn overview_grid(w: u16, h: u16, n: usize) -> GridPlan {
    if n == 0 {
        return GridPlan {
            cols: 0,
            rows: 0,
            visible: 0,
            row_height: 0,
        };
    }
    // Columns: as many preferred-width tiles as fit, clamped to [1, n].
    let cols = (w / TILE_PREF_W).clamp(1, n as u16);
    let rows_needed = (n as u16).div_ceil(cols);
    // Fixed tile height, capped so tiles stay landscape rather than filling
    // a tall terminal; never below the readable minimum.
    let row_height = (h / rows_needed).clamp(TILE_MIN_H, TILE_MAX_H);
    // How many of those rows actually fit in the available height.
    let rows_fit = (h / row_height).max(1);
    let rows = rows_needed.min(rows_fit);
    let visible = n.min((cols as usize) * (rows as usize));
    GridPlan {
        cols,
        rows,
        visible,
        row_height,
    }
}

/// Whether the section browser can show both panes side-by-side, and the
/// nav pane width if so.
#[derive(Debug, PartialEq, Eq)]
pub enum BrowserSplit {
    /// Side-by-side; value is nav pane width in columns.
    Dual(u16),
    /// Too narrow: single pane (caller toggles which).
    Single,
}

pub const NAV_MIN_W: u16 = 20;
pub const CHART_MIN_W: u16 = 30;

/// Decide the browser split for a given width.
pub fn browser_split(w: u16) -> BrowserSplit {
    if w >= NAV_MIN_W + CHART_MIN_W {
        // Nav gets ~30%, clamped to a sane band.
        let nav = ((w as u32 * 30 / 100) as u16).clamp(NAV_MIN_W, 40);
        BrowserSplit::Dual(nav)
    } else {
        BrowserSplit::Single
    }
}

/// True when the terminal is too small to render anything useful.
pub fn too_small(w: u16, h: u16) -> bool {
    w < MIN_WIDTH || h < MIN_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_columns_scale_with_width_not_maximized() {
        // Preferred width 48: ~one column per 48 cells, so tiles stay wide
        // instead of being packed into as many narrow strips as fit.
        assert_eq!(overview_grid(60, 40, 6).cols, 1);
        assert_eq!(overview_grid(120, 40, 6).cols, 2);
        assert_eq!(overview_grid(200, 40, 6).cols, 4);
        // Never more columns than tiles.
        assert!(overview_grid(1000, 40, 6).cols <= 6);
    }

    #[test]
    fn grid_row_height_is_capped_landscape() {
        // A tall terminal must not stretch tiles into vertical strips.
        let p = overview_grid(120, 100, 6);
        assert!(p.row_height <= TILE_MAX_H, "row_height {}", p.row_height);
        assert!(p.row_height >= TILE_MIN_H);
    }

    #[test]
    fn grid_fits_all_when_room() {
        let p = overview_grid(200, 40, 6);
        assert_eq!(p.visible, 6);
    }

    #[test]
    fn grid_drops_tiles_when_too_short() {
        // 60 wide => 1 col; a short terminal can only show a couple of the
        // stacked tiles, so the rest are dropped rather than crushed.
        let p = overview_grid(60, 16, 6);
        assert_eq!(p.cols, 1);
        assert!(p.visible < 6);
        assert_eq!(p.visible, (p.cols * p.rows) as usize);
    }

    #[test]
    fn grid_single_column_when_narrow() {
        let p = overview_grid(40, 30, 6);
        assert_eq!(p.cols, 1);
    }

    #[test]
    fn browser_dual_when_wide() {
        assert!(matches!(browser_split(120), BrowserSplit::Dual(_)));
    }

    #[test]
    fn browser_single_when_narrow() {
        assert_eq!(browser_split(40), BrowserSplit::Single);
    }

    #[test]
    fn too_small_below_minimums() {
        assert!(too_small(20, 20));
        assert!(too_small(80, 8));
        assert!(!too_small(80, 24));
    }
}
