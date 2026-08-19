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
pub(crate) struct GroupWindowSlot {
    // even = stable, odd = write in progress; 0 = never stamped. Parity
    // survives u64 wrap (2^64 is even); a wrap that lands exactly on 0
    // degrades to "no window" for one cycle, never a wrong window.
    seq: AtomicU64,
    begin_ns: AtomicU64,
    end_ns: AtomicU64,
}

impl GroupWindowSlot {
    // Not yet called from production code: the only caller is
    // `AcquisitionGroup::new()`, itself unreached until a sampler migrates
    // (see the note there).
    #[allow(dead_code)]
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
    //
    /// # Single-writer contract
    ///
    /// This slot supports exactly one concurrent writer. The seqlock's
    /// odd/even parity is what lets readers detect a torn write; two
    /// writers racing each other can each observe an even `seq`, both bump
    /// it odd, and interleave their field stores before either bumps it
    /// back even — the result is a torn window (a begin from one writer
    /// paired with an end from the other) behind a seq value that reads as
    /// perfectly stable. There is no retry that catches this: it is not a
    /// transient tear a reader spins past, it is a permanently wrong pair.
    /// A sampler that drives its acquisition from more than one task (for
    /// example a drivehealth-style sampler with a blocking probe task
    /// alongside `refresh()`) must stamp the group from that one task only,
    /// never from both.
    // Not yet called from production code — the only caller is
    // `AcquisitionGuard::finish()`, unreached until a sampler migrates (see
    // the note on `AcquisitionGroup::new()`).
    #[allow(dead_code)]
    pub(crate) fn store(&self, w: Window) {
        let s = self.seq.load(Ordering::Relaxed);
        debug_assert_eq!(
            s & 1,
            0,
            "GroupWindowSlot has a single writer; concurrent store() detected"
        );
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed); // odd: in progress
        std::sync::atomic::fence(Ordering::Release); // field stores stay below the odd store
        self.begin_ns.store(w.begin_ns, Ordering::Relaxed);
        self.end_ns.store(w.end_ns, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release); // even: stable
    }

    /// Retries on a torn read with no bound or backoff. That's acceptable
    /// here because the writer's critical section is nanoseconds long
    /// against a millisecond-scale sampler tick — the writer's duty cycle
    /// makes an unlucky reader spin at most a couple of iterations.
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
pub(crate) struct AcquisitionGroup {
    pub sampler: &'static str,
    pub name: &'static str,
    slot: GroupWindowSlot,
}

impl AcquisitionGroup {
    // Not yet called from production code: no sampler constructs an
    // `AcquisitionGroup` until it migrates to a declared group (that's a
    // per-sampler follow-up, not this task). The V3 snapshot builder only
    // reads groups that samplers have already registered on
    // `ACQUISITION_GROUPS`; it never constructs one itself.
    #[allow(dead_code)]
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
    ///
    /// On the success path, call `finish()` once values are set. On an
    /// error path, call `discard()` (or just let `?` return and drop the
    /// guard — the two are equivalent). Dropping the guard without
    /// `finish()` does NOT stamp: the group keeps whatever window it had
    /// before, which readers see as "nothing new" this tick. That is a
    /// deliberate choice — see [`AcquisitionGuard`] for the reasoning.
    ///
    /// # Single-writer contract
    ///
    /// A group has exactly one writer. Calling `acquire()` concurrently
    /// from two tasks for the same group is a bug: both guards stamp the
    /// same underlying [`GroupWindowSlot`], and two interleaved `store()`s
    /// can produce a torn window that reads as perfectly valid (see
    /// [`GroupWindowSlot::store`]) — there is no retry that catches it. A
    /// sampler whose reads span more than one task (a drivehealth-style
    /// blocking probe running alongside `refresh()`) must have that one
    /// task own the group and call `acquire()`/`finish()`, not `refresh()`
    /// as well.
    // Not yet called from production code — see the note on `new()`. Only
    // exercised by the V3 snapshot builder's own tests today, stamping a
    // synthetic group to exercise the declared-group path.
    #[allow(dead_code)]
    pub(crate) fn acquire(&self) -> AcquisitionGuard<'_> {
        AcquisitionGuard::begin(&self.slot)
    }

    /// Read the group's current window, if it has ever been stamped. Used
    /// by the V3 snapshot builder; read-only by design — reaching around
    /// this and storing into the slot directly would bypass the guard's
    /// stamp-last discipline.
    pub(crate) fn window(&self) -> Option<Window> {
        self.slot.load()
    }
}

/// Begin-marker for a group read section, same clock discipline as [`timed`].
///
/// `finish()` is the ONLY path that stamps the window into the slot.
/// `discard()` (or an ordinary drop, e.g. via a `?`-return) consumes the
/// guard WITHOUT stamping.
///
/// This is a deliberate asymmetry with `timed`/`Acquisition`, which stamp
/// unconditionally: a group's values live in ordinary metric storage that
/// a failed read may have left untouched from the previous tick, so
/// stamping on drop would pair last tick's values with THIS tick's window
/// — a confident, wrong observation for an interval nothing actually
/// measured. Not stamping leaves the group's window exactly where it was;
/// readers see "no new data this tick", which is the honest signal. A group
/// whose reads keep failing simply stops advancing, visibly, in the data —
/// missing beats wrong.
// consumed by the V3 snapshot builder (next task)
#[allow(dead_code)]
pub(crate) struct AcquisitionGuard<'a> {
    slot: &'a GroupWindowSlot,
    begin_ns: u64,
    begin_mono: Instant,
}

#[allow(dead_code)] // consumed by the V3 snapshot builder (next task)
impl<'a> AcquisitionGuard<'a> {
    pub(crate) fn begin(slot: &'a GroupWindowSlot) -> Self {
        Self {
            slot,
            begin_ns: now_wall_ns(),
            begin_mono: Instant::now(),
        }
    }

    /// Stamp begin (captured at `acquire()`) through begin + monotonic
    /// elapsed into the slot. Call this once the read section succeeded and
    /// values are set — stamp-last, so a racing reader never pairs a value
    /// with a window that outran it.
    pub(crate) fn finish(self) {
        let elapsed = self.begin_mono.elapsed().as_nanos() as u64;
        self.slot
            .store(Window::new(self.begin_ns, self.begin_ns + elapsed));
    }

    /// Consume the guard without stamping. Use on an error path to make the
    /// "no update this tick" intent explicit at the call site; a bare
    /// `?`-return that drops the guard has the identical effect.
    pub(crate) fn discard(self) {}
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

    // This test's value depends on two things that must not be weakened:
    // an arm64 runner in CI (x86's TSO memory model hides this bug class —
    // it was invisible there even when the release/acquire fences were
    // missing) and >=5M reader iterations in a debug build. Measured by the
    // reviewer on the CI's only weak-memory runner (macos-latest, arm64,
    // debug build): the pre-fix tear was caught 0/30 runs at 200k
    // iterations, but 5/5 at 5M — and 5M costs ~0.42s on correct code. Do
    // not shrink either the iteration count or drop the arm64 requirement.
    #[test]
    fn group_slot_is_tear_free_under_contention() {
        // Writer stamps (n, n+1) pairs; readers must never observe a mixed pair.
        let slot = std::sync::Arc::new(GroupWindowSlot::new());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let ws = slot.clone();
        let wstop = stop.clone();
        let wbarrier = barrier.clone();
        let writer = std::thread::spawn(move || {
            wbarrier.wait();
            let mut n = 0u64;
            while !wstop.load(std::sync::atomic::Ordering::Relaxed) {
                ws.store(Window::new(n, n + 1));
                n += 2;
            }
        });

        barrier.wait();
        let mut observed = 0u64; // Some(_) loads: contention actually exercised
        let mut advanced = 0u64; // distinct begin_ns values: writer made progress
        let mut last_begin = None;
        for _ in 0..5_000_000 {
            if let Some(w) = slot.load() {
                assert_eq!(w.end_ns, w.begin_ns + 1, "torn read: {w:?}");
                observed += 1;
                if last_begin != Some(w.begin_ns) {
                    advanced += 1;
                    last_begin = Some(w.begin_ns);
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();

        assert!(
            observed > 1_000_000,
            "reader saw too few stamps ({observed}) — not exercising contention"
        );
        assert!(
            advanced > 100,
            "writer made no visible progress ({advanced}) — vacuous run"
        );
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
    fn acquisition_guard_drop_discards() {
        static SLOT: GroupWindowSlot = GroupWindowSlot::new();
        {
            let _acq = AcquisitionGuard::begin(&SLOT);
            // early return / ? path: guard dropped without finish()
        }
        assert!(
            SLOT.load().is_none(),
            "drop must not stamp: a failed read leaves the slot exactly as it was"
        );
    }
}
