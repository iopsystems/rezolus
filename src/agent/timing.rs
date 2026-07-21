//! Shared acquisition-window timing for Regime-R samplers (read-at-refresh).
//! Captures wall-clock begin + monotonic width so an NTP step during the read
//! cannot corrupt the window. See
//! `docs/journal/2026-07-10-all-sampler-observation-windows.md`.

use metriken::Window;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Wall-clock nanoseconds since the Unix epoch, saturating to 0 before it.
fn now_wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Run `read` while capturing its acquisition window: begin is wall time before
/// the call; end is begin + monotonic elapsed (immune to an NTP step during the
/// read). Use for a single read block (a drivehealth ioctl, a `/proc` file read).
pub(crate) fn timed<T>(read: impl FnOnce() -> T) -> (T, Window) {
    let begin_ns = now_wall_ns();
    let begin_mono = Instant::now();
    let out = read();
    let elapsed_ns = begin_mono.elapsed().as_nanos() as u64;
    (out, Window::new(begin_ns, begin_ns + elapsed_ns))
}

/// A begin-marker for stamping several reads/writes that are interleaved (e.g. a
/// per-CPU sweep, or a GPU device loop that reads-and-sets per metric). `begin()`
/// captures wall + monotonic start; each `window()` closes at the current instant
/// (begin + monotonic elapsed), so entries stamped later carry a marginally wider
/// window — honest, since they were read later.
pub(crate) struct Acquisition {
    begin_ns: u64,
    begin_mono: Instant,
}

impl Acquisition {
    pub(crate) fn begin() -> Self {
        Self {
            begin_ns: now_wall_ns(),
            begin_mono: Instant::now(),
        }
    }

    pub(crate) fn window(&self) -> Window {
        let elapsed_ns = self.begin_mono.elapsed().as_nanos() as u64;
        Window::new(self.begin_ns, self.begin_ns + elapsed_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timed_captures_a_nonzero_window_covering_the_read() {
        let (val, window) = timed(|| {
            std::thread::sleep(Duration::from_millis(5));
            7
        });
        assert_eq!(val, 7);
        assert!(window.end_ns >= window.begin_ns);
        assert!(
            window.width_ns() >= 4_000_000,
            "≥4ms: {}",
            window.width_ns()
        );
    }

    #[test]
    fn timed_begin_is_wallclock_after_the_epoch() {
        let (_, window) = timed(|| 0);
        assert!(window.begin_ns > 0, "wall-clock begin recorded");
    }

    #[test]
    fn acquisition_window_covers_from_begin_to_now() {
        let acq = Acquisition::begin();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let w = acq.window();
        assert!(w.begin_ns > 0);
        assert!(w.width_ns() >= 2_000_000, "≥2ms: {}", w.width_ns());
    }
}
