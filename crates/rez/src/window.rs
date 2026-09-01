//! One reading's acquisition window.

/// The interval a value was acquired over: `[begin_ns, end_ns)`, nanoseconds
/// since the Unix epoch.
///
/// Structurally identical to `metriken::Window`, and deliberately not it. The
/// window is part of what a `.rez` segment *stores* — the query engine reads
/// `<m>:window_begin`/`<m>:window_width` back out to bound `rate()` — so the
/// archive format owns the type. Borrowing the agent's in-memory one coupled
/// the file format to a metrics registry it has no other reason to know
/// about, and that registry (`metriken-core`) declares a `linkme`
/// distributed slice, which has no wasm32 implementation: the coupling alone
/// is what kept a `.rez` unreadable in the browser.
///
/// Conversion from the agent's type happens once, at ingest — see the
/// `From` impl below, compiled only with the `write` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Window {
    /// Start of the acquisition interval (ns since Unix epoch).
    pub begin_ns: u64,
    /// End of the acquisition interval (ns since Unix epoch).
    pub end_ns: u64,
}

impl Window {
    /// Construct a window from begin/end nanoseconds.
    pub const fn new(begin_ns: u64, end_ns: u64) -> Self {
        Self { begin_ns, end_ns }
    }

    /// Width in nanoseconds; saturating, so an end before its begin reads 0
    /// rather than wrapping to a ~584-year window.
    pub const fn width_ns(&self) -> u64 {
        self.end_ns.saturating_sub(self.begin_ns)
    }
}

#[cfg(feature = "write")]
impl From<metriken::Window> for Window {
    fn from(w: metriken::Window) -> Self {
        Self {
            begin_ns: w.begin_ns,
            end_ns: w.end_ns,
        }
    }
}

/// The agent's window, from the archive's — the inverse of the ingest
/// conversion, for code that hands a stored window back to a snapshot type
/// (fixtures, mostly).
#[cfg(feature = "write")]
impl From<Window> for metriken::Window {
    fn from(w: Window) -> Self {
        metriken::Window::new(w.begin_ns, w.end_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_end_minus_begin() {
        let w = Window::new(1_000, 3_500);
        assert_eq!(w.width_ns(), 2_500);
    }

    /// An inverted window is a producer bug, not a reason to hand the query
    /// engine a rate divisor of ~584 years.
    #[test]
    fn an_inverted_window_saturates_to_zero_width() {
        assert_eq!(Window::new(3_500, 1_000).width_ns(), 0);
    }

    /// The archive's window and the agent's must stay the same two numbers:
    /// a segment written from a converted window has to mean what the agent
    /// measured.
    #[cfg(feature = "write")]
    #[test]
    fn conversion_from_the_agents_window_preserves_both_edges() {
        let w: Window = metriken::Window::new(1_000, 3_500).into();
        assert_eq!(w, Window::new(1_000, 3_500));
    }
}
