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
}
