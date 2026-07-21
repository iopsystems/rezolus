//! Time window selection for TUI charts.

/// A selectable look-back window. `All` uses the full data extent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeWindow {
    Last1m,
    Last5m,
    Last15m,
    All,
}

impl TimeWindow {
    /// Ordered cycle used by the `[` / `]` keys.
    pub const ORDER: [TimeWindow; 4] = [
        TimeWindow::Last1m,
        TimeWindow::Last5m,
        TimeWindow::Last15m,
        TimeWindow::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::Last1m => "1m",
            TimeWindow::Last5m => "5m",
            TimeWindow::Last15m => "15m",
            TimeWindow::All => "all",
        }
    }

    fn span_secs(self) -> Option<f64> {
        match self {
            TimeWindow::Last1m => Some(60.0),
            TimeWindow::Last5m => Some(300.0),
            TimeWindow::Last15m => Some(900.0),
            TimeWindow::All => None,
        }
    }

    /// Grow (`]`) toward `All`; saturates at the ends.
    pub fn grow(self) -> TimeWindow {
        let i = Self::ORDER.iter().position(|w| *w == self).unwrap_or(0);
        Self::ORDER[(i + 1).min(Self::ORDER.len() - 1)]
    }

    /// Shrink (`[`) toward `Last1m`; saturates at the ends.
    pub fn shrink(self) -> TimeWindow {
        let i = Self::ORDER.iter().position(|w| *w == self).unwrap_or(0);
        Self::ORDER[i.saturating_sub(1)]
    }

    /// Resolve to `(start_s, end_s, step_s)` given the source's full extent
    /// and native interval. `end` is always the extent's max. `start` is
    /// clamped to the extent min. Returns `None` if the extent is empty.
    pub fn resolve(self, extent: Option<(f64, f64)>, interval_s: f64) -> Option<(f64, f64, f64)> {
        let (min, max) = extent?;
        let step = if interval_s > 0.0 { interval_s } else { 1.0 };
        let start = match self.span_secs() {
            Some(span) => (max - span).max(min),
            None => min,
        };
        Some((start, max, step))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_and_shrink_saturate() {
        assert_eq!(TimeWindow::Last1m.shrink(), TimeWindow::Last1m);
        assert_eq!(TimeWindow::All.grow(), TimeWindow::All);
        assert_eq!(TimeWindow::Last1m.grow(), TimeWindow::Last5m);
        assert_eq!(TimeWindow::Last5m.shrink(), TimeWindow::Last1m);
    }

    #[test]
    fn resolve_clamps_start_to_extent() {
        // extent only 30s wide, but window is 60s: start clamps to min.
        let r = TimeWindow::Last1m.resolve(Some((100.0, 130.0)), 1.0).unwrap();
        assert_eq!(r, (100.0, 130.0, 1.0));
    }

    #[test]
    fn resolve_all_spans_full_extent() {
        let r = TimeWindow::All.resolve(Some((100.0, 1000.0)), 2.0).unwrap();
        assert_eq!(r, (100.0, 1000.0, 2.0));
    }

    #[test]
    fn resolve_windowed_uses_span_from_end() {
        let r = TimeWindow::Last5m.resolve(Some((0.0, 1000.0)), 1.0).unwrap();
        assert_eq!(r, (700.0, 1000.0, 1.0));
    }

    #[test]
    fn resolve_empty_extent_is_none() {
        assert_eq!(TimeWindow::All.resolve(None, 1.0), None);
    }
}
