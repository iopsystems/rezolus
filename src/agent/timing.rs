//! Shared acquisition-window timing for Regime-R samplers (read-at-refresh).
//! Captures wall-clock begin + monotonic width so an NTP step during the read
//! cannot corrupt the window. See
//! `docs/journal/2026-07-10-all-sampler-observation-windows.md`.

use metriken::Window;
use std::sync::atomic::{AtomicU64, Ordering};
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
// consumers are Linux samplers today
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
// consumers are Linux samplers today
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Acquisition {
    begin_ns: u64,
    begin_mono: Instant,
}

// consumers are Linux samplers today
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// A lock-free window slot shared between a sampler's acquisition path and
/// the snapshot builder: a seqlock over (begin_ns, end_ns). One writer (the
/// sampler's refresh/blocking task) and any number of readers. Readers retry
/// on a torn read; the worst case for a racing scrape is pairing values with
/// the previous tick's window (the design's stamp-last rule: samplers set
/// values first, stamp the window last, so the window can only lag, never
/// lead).
// consumed by the V3 snapshot builder (next task)
#[allow(dead_code)]
pub(crate) struct GroupWindowSlot {
    seq: AtomicU64, // even = stable, odd = write in progress; 0 = never stamped
    begin_ns: AtomicU64,
    end_ns: AtomicU64,
}

#[allow(dead_code)] // consumed by the V3 snapshot builder (next task)
impl GroupWindowSlot {
    pub(crate) const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            begin_ns: AtomicU64::new(0),
            end_ns: AtomicU64::new(0),
        }
    }

    // The fences below are load-bearing on weak-memory targets (arm64): a
    // `Release` STORE only orders accesses that come BEFORE it, so it cannot
    // pin the field stores that come after the odd `seq` store — without the
    // release fence they may become visible first, and a reader can pass both
    // sequence checks around a torn pair. Symmetrically, an `Acquire` LOAD
    // only orders accesses AFTER it, so the field reads need the acquire
    // fence to keep them from sinking below the validation load. The
    // contention test caught exactly this tear on an arm64 host when the
    // fences were plain Release/Acquire orderings on the seq accesses; x86's
    // stronger model hides it.
    pub(crate) fn store(&self, w: Window) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed); // odd: in progress
        std::sync::atomic::fence(Ordering::Release); // field stores stay below the odd store
        self.begin_ns.store(w.begin_ns, Ordering::Relaxed);
        self.end_ns.store(w.end_ns, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release); // even: stable
    }

    pub(crate) fn load(&self) -> Option<Window> {
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 == 0 {
                return None; // never stamped
            }
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue; // write in progress
            }
            let begin = self.begin_ns.load(Ordering::Relaxed);
            let end = self.end_ns.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire); // field reads stay above the validation load
            if self.seq.load(Ordering::Relaxed) == s1 {
                return Some(Window::new(begin, end));
            }
        }
    }
}

/// One declared acquisition group: `<sampler>/<name>` plus its window slot.
/// Samplers declare these as statics and register them on
/// [`crate::agent::samplers::ACQUISITION_GROUPS`]; the V3 snapshot builder
/// enumerates the slice to assemble SnapshotV3 groups. The name `main` is
/// reserved for the builder's transitional default groups.
// consumed by the V3 snapshot builder (next task)
#[allow(dead_code)]
pub(crate) struct AcquisitionGroup {
    pub sampler: &'static str,
    pub name: &'static str,
    pub slot: GroupWindowSlot,
}

#[allow(dead_code)] // consumed by the V3 snapshot builder (next task)
impl AcquisitionGroup {
    pub(crate) const fn new(sampler: &'static str, name: &'static str) -> Self {
        Self {
            sampler,
            name,
            slot: GroupWindowSlot::new(),
        }
    }

    /// Bracket one read section. Set values between `acquire()` and
    /// `finish()`; `finish()` stamps the window LAST so a racing scrape can
    /// pair values with the previous window but never with a future one.
    pub(crate) fn acquire(&self) -> AcquisitionGuard<'_> {
        AcquisitionGuard::begin(&self.slot)
    }
}

/// Begin-marker for a group read section; `finish()` (or drop) stamps
/// wall-begin + monotonic-elapsed into the slot, same clock discipline as
/// [`timed`].
// consumed by the V3 snapshot builder (next task)
#[allow(dead_code)]
pub(crate) struct AcquisitionGuard<'a> {
    slot: &'a GroupWindowSlot,
    begin_ns: u64,
    begin_mono: Instant,
    finished: bool,
}

#[allow(dead_code)] // consumed by the V3 snapshot builder (next task)
impl<'a> AcquisitionGuard<'a> {
    pub(crate) fn begin(slot: &'a GroupWindowSlot) -> Self {
        Self {
            slot,
            begin_ns: now_wall_ns(),
            begin_mono: Instant::now(),
            finished: false,
        }
    }

    pub(crate) fn finish(mut self) {
        self.stamp();
    }

    fn stamp(&mut self) {
        if !self.finished {
            self.finished = true;
            let elapsed = self.begin_mono.elapsed().as_nanos() as u64;
            self.slot
                .store(Window::new(self.begin_ns, self.begin_ns + elapsed));
        }
    }
}

impl Drop for AcquisitionGuard<'_> {
    fn drop(&mut self) {
        self.stamp(); // an early-return read section still gets an honest window
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

    #[test]
    fn group_slot_roundtrips_a_window() {
        let slot = GroupWindowSlot::new();
        assert_eq!(slot.load(), None, "unstamped slot is empty");
        let w = Window::new(1_000, 2_000);
        slot.store(w);
        assert_eq!(slot.load(), Some(w));
        let w2 = Window::new(3_000, 4_000);
        slot.store(w2);
        assert_eq!(slot.load(), Some(w2), "latest stamp wins");
    }

    #[test]
    fn group_slot_is_tear_free_under_contention() {
        // Writer stamps (n, n+1) pairs; readers must never observe a mixed pair.
        let slot = std::sync::Arc::new(GroupWindowSlot::new());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ws = slot.clone();
        let wstop = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut n = 0u64;
            while !wstop.load(std::sync::atomic::Ordering::Relaxed) {
                ws.store(Window::new(n, n + 1));
                n += 2;
            }
        });
        for _ in 0..200_000 {
            if let Some(w) = slot.load() {
                assert_eq!(w.end_ns, w.begin_ns + 1, "torn read: {w:?}");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
    }

    #[test]
    fn acquisition_guard_stamps_on_finish() {
        static SLOT: GroupWindowSlot = GroupWindowSlot::new();
        let acq = AcquisitionGuard::begin(&SLOT);
        std::thread::sleep(std::time::Duration::from_millis(3));
        acq.finish();
        let w = SLOT.load().expect("finish() stamped the slot");
        assert!(w.begin_ns > 0, "wall-clock begin");
        assert!(w.width_ns() >= 2_000_000, ">=2ms width: {}", w.width_ns());
    }

    #[test]
    fn acquisition_guard_stamps_on_drop() {
        static SLOT: GroupWindowSlot = GroupWindowSlot::new();
        {
            let _acq = AcquisitionGuard::begin(&SLOT);
            // early return / ? path: guard dropped without finish()
        }
        assert!(SLOT.load().is_some(), "drop stamps an honest window");
    }
}
