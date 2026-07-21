//! Pure responsive layout math. No ratatui types so it is trivially
//! unit-testable; render code consumes these decisions.

/// Minimum terminal size below which we show a "too small" message.
pub const MIN_WIDTH: u16 = 30;
pub const MIN_HEIGHT: u16 = 10;

/// Per-tile minimum cell size on the overview grid.
pub const TILE_MIN_W: u16 = 24;
pub const TILE_MIN_H: u16 = 6;

/// Overview grid decision: how many columns, and how many tiles fit.
#[derive(Debug, PartialEq, Eq)]
pub struct GridPlan {
    pub cols: u16,
    pub rows: u16,
    pub visible: usize,
}

/// Compute the overview grid for `n` priority-ordered tiles in a `w`x`h`
/// area. Drops lowest-priority tiles that do not fit vertically.
pub fn overview_grid(w: u16, h: u16, n: usize) -> GridPlan {
    if n == 0 {
        return GridPlan { cols: 0, rows: 0, visible: 0 };
    }
    let cols = (w / TILE_MIN_W).max(1);
    let max_rows = (h / TILE_MIN_H).max(1);
    let capacity = (cols as usize) * (max_rows as usize);
    let visible = n.min(capacity);
    let rows = ((visible as u16) + cols - 1) / cols;
    GridPlan { cols, rows, visible }
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
    fn grid_drops_tiles_that_dont_fit() {
        // 50 wide => 2 cols; 12 high => 2 rows => capacity 4. 6 tiles => 4 visible.
        let p = overview_grid(50, 12, 6);
        assert_eq!(p.cols, 2);
        assert_eq!(p.visible, 4);
        assert_eq!(p.rows, 2);
    }

    #[test]
    fn grid_fits_all_when_room() {
        let p = overview_grid(120, 30, 6);
        assert_eq!(p.visible, 6);
        assert!(p.cols >= 4);
    }

    #[test]
    fn grid_single_column_when_narrow() {
        let p = overview_grid(24, 30, 6);
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
