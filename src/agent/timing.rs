//! Shared acquisition-window timing for Regime-R samplers (read-at-refresh).
//! Captures wall-clock begin + monotonic width so an NTP step during the read
//! cannot corrupt the window. See
//! `docs/journal/2026-07-10-all-sampler-observation-windows.md`.
//!
//! Every Regime-R sampler now brackets its read section with a declared
//! [`AcquisitionGroup`] (`acquire()`/`finish()`/`discard()`) rather than the
//! ungrouped, per-call `timed()`/`Acquisition` helpers this module used to
//! export — the last two callers (`drivehealth` and the GPU samplers)
//! migrated in Stage 3c wave 2 Part B (see `docs/principles.md` principle
//! 18). Both were removed once that landed: a windowless per-metric stamp
//! disconnected from any registered group no longer has a caller, and
//! keeping unused public timing primitives around invites a new one to
//! reappear outside the group discipline.

use metriken::Window;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Wall-clock nanoseconds since the Unix epoch, saturating to 0 before it.
fn now_wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
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
    // Only reachable from the Linux-only BPF sampler refresh path (via
    // AcquisitionGuard::finish); non-Linux builds compile this file (stats.rs
    // constructs AcquisitionGroup statics for cross-platform metric identity)
    // but never drive a refresh, so store() is genuinely unused there.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
    // 0 = unbounded (walk the full backing array's `entries()`); nonzero =
    // the real member-population bound. See `set_member_bound`/`member_bound`.
    member_bound: AtomicUsize,
    // Init-time flag: true means this group's acquisition IS the exposition
    // read itself (mmap-direct `PackedCounters`), not a sampler `refresh()`.
    // See `set_reader_stamped`/`is_reader_stamped`.
    reader_stamped: AtomicBool,
}

impl AcquisitionGroup {
    /// Declare a group. The V3 snapshot builder only reads groups that
    /// samplers have already registered on `ACQUISITION_GROUPS`; it never
    /// constructs one itself.
    pub(crate) const fn new(sampler: &'static str, name: &'static str) -> Self {
        Self {
            sampler,
            name,
            slot: GroupWindowSlot::new(),
            member_bound: AtomicUsize::new(0),
            reader_stamped: AtomicBool::new(false),
        }
    }

    /// Declare a group that is reader-stamped from the moment its `static`
    /// initializes — before `main` runs, independent of whether the
    /// specific `PackedCounters` instance that would otherwise call
    /// `set_reader_stamped()` ever actually constructs.
    ///
    /// `PackedCounters::new` ALSO calls `set_reader_stamped()` on its group
    /// (idempotent, harmless here) — that call alone is not sufficient by
    /// itself. It only runs once a sampler's `init()` has actually gotten
    /// far enough to attach a live BPF map, which does not happen when the
    /// sampler is disabled via config, and does not happen at all outside
    /// a running agent (unit tests construct snapshots directly, without
    /// ever calling any sampler's `init()`). A `#[metric]` static's
    /// registration — and therefore its `acq_group` tag and membership in
    /// `create`/`create_v3`'s walk — is compile-time and unconditional,
    /// independent of the sampler's runtime/config state; a group that
    /// stayed "declared but not yet known reader-stamped" in that gap
    /// would fall through to the sampler-stamped declared-group path,
    /// which walks `0..entries()` (or `0..member_bound`) and — since a
    /// declared group's unpopulated members intentionally emit `None`
    /// rather than being skipped (see the CounterGroup/GaugeGroup arms'
    /// doc comments) — pushes one entry per capacity slot. That is a
    /// bounded, cheap cost for a `CpuCounters`-scale bound (≤`MAX_CPUS` =
    /// 1024) but was measured to allocate ~4.2M entries and exhaust an
    /// 8 GB container for `task_cpu_usage` (`MAX_PID` = 4,194,304) when
    /// this gap was hit in a test that never ran `cpu_usage`'s BPF
    /// sampler. Declaring the group's statics with this constructor
    /// instead of `new()` closes the gap entirely, for every packed/sparse
    /// group regardless of `MAX_PID`/`MAX_CGROUPS` scale.
    // Only meaningfully reachable via the Linux-only PackedCounters path,
    // but the const value itself is harmless (and correctly inert) to
    // construct on any platform — see the read-side note on
    // `is_reader_stamped`.
    pub(crate) const fn new_reader_stamped(sampler: &'static str, name: &'static str) -> Self {
        Self {
            sampler,
            name,
            slot: GroupWindowSlot::new(),
            member_bound: AtomicUsize::new(0),
            reader_stamped: AtomicBool::new(true),
        }
    }

    /// Set the group's member-population bound: the real number of
    /// populated members (e.g. `possible_cpus()` for a per-CPU group), as
    /// opposed to the backing array's `entries()` capacity — a fixed
    /// implementation ceiling (`MAX_CPUS`), not a population count (see
    /// `docs/principles.md` principle 6: "over-allocates on small
    /// machines"). The V3 snapshot builder walks only `0..bound` for a
    /// group that has one set, instead of the full `entries()`, so an
    /// ~18-CPU host does not emit 1024 mostly-empty slots per tick.
    ///
    /// Expected to be called exactly once, at sampler init, before any
    /// snapshot walk reads it — the population is boot-fixed (CPUs coming
    /// online later are still within the possible-CPU bound computed at
    /// init). This is not a documented multi-writer API: a second call
    /// silently overwrites the first (last-write-wins), which is fine for
    /// the single-init contract but would not be safe as a runtime toggle.
    // Only called from `CpuCounters::new`, Linux-only; see the note on
    // `GroupWindowSlot::store`. `member_bound()` itself (the read side) is
    // NOT guarded the same way — the V3 snapshot builder reads it
    // unconditionally on every platform, whether or not anything ever set it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn set_member_bound(&self, n: usize) {
        self.member_bound.store(n, Ordering::Relaxed);
    }

    /// The group's member-population bound, if one has been set (`None`
    /// means unbounded: walk the full `entries()`).
    pub(crate) fn member_bound(&self) -> Option<usize> {
        match self.member_bound.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    /// Mark this group as reader-stamped: its acquisition window is
    /// stamped by the snapshot builder's exposition-time read (a
    /// `PackedCounters` mmap-direct group), not by any sampler's
    /// `refresh()`. `refresh()` never calls `acquire()`/`finish()` for
    /// these groups — `PackedCounters::refresh()` is a no-op, since values
    /// live directly in the attached mmap and are read on demand by
    /// `create`/`create_v3`. The single writer for a reader-stamped
    /// group's window slot is therefore the builder task itself (see
    /// `docs/principles.md` principle 18's single-writer contract, and
    /// `GroupWindowSlot::store`'s doc comment) — safe because the snapshot
    /// builder is invoked serially (guarded by a mutex upstream), never
    /// concurrently with itself.
    ///
    /// Expected to be called exactly once per group, at sampler init
    /// (`PackedCounters::new`) — the same single-init contract as
    /// `set_member_bound`. Calling it more than once (e.g. two
    /// `.packed_counters()` calls sharing one like-entities group, or a
    /// group already declared with [`new_reader_stamped`](Self::new_reader_stamped))
    /// is idempotent and harmless. Prefer declaring the group with
    /// `new_reader_stamped` in the first place — see its doc comment for
    /// why relying on this call alone leaves a gap.
    // Only called from `PackedCounters::new`, Linux-only; see the note on
    // `GroupWindowSlot::store`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn set_reader_stamped(&self) {
        self.reader_stamped.store(true, Ordering::Relaxed);
    }

    /// Whether this group is reader-stamped (see `set_reader_stamped`).
    /// Read unconditionally by `create`/`create_v3` on every platform,
    /// whether or not anything ever set it (mirrors `member_bound`'s
    /// read-side note above).
    pub(crate) fn is_reader_stamped(&self) -> bool {
        self.reader_stamped.load(Ordering::Relaxed)
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
    // Only reachable from the Linux-only BPF sampler refresh path; see the
    // note on `GroupWindowSlot::store`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
// Only constructed from the Linux-only BPF sampler refresh path (via
// AcquisitionGroup::acquire); see the note on `GroupWindowSlot::store`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct AcquisitionGuard<'a> {
    slot: &'a GroupWindowSlot,
    begin_ns: u64,
    begin_mono: Instant,
    // Set by `mark_end()`; when present, `finish()` publishes THIS end
    // instead of re-deriving it from elapsed-at-publish-time. See
    // `mark_end`'s doc comment for why the two can differ.
    marked_end_ns: Option<u64>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl<'a> AcquisitionGuard<'a> {
    pub(crate) fn begin(slot: &'a GroupWindowSlot) -> Self {
        Self {
            slot,
            begin_ns: now_wall_ns(),
            begin_mono: Instant::now(),
            marked_end_ns: None,
        }
    }

    /// Record the window's end AT THIS INSTANT — the moment the group's
    /// last member value was actually read — instead of letting `finish()`
    /// derive it from elapsed-at-publish-time.
    ///
    /// Publication order is unchanged: `finish()` still runs after this,
    /// still stamps last, still gives a racing reader the same "can only
    /// lag, never lead" guarantee (stamp-last constrains WHEN the slot
    /// becomes visible, not what END value it carries). What changes is
    /// the CONTENT of the published window.
    ///
    /// This matters specifically for reader-stamped groups
    /// (`PackedCounters` mmap-direct — see `AcquisitionGroup::set_reader_stamped`):
    /// their bracket spans `acquire()` at the group's first member touch
    /// through `finish()`, which in `create`/`create_v3` runs at the
    /// group's emit point — AFTER the rest of that tick's registry walk
    /// (every other group's schema assembly, hashing, etc.) has also run.
    /// Without `mark_end()`, that walk time gets counted INTO the window's
    /// width, inflating a genuine microsecond-scale read span into a
    /// millisecond-scale one that has nothing to do with how long this
    /// group's own values took to read. Calling `mark_end()` immediately
    /// after the group's member-value loop — before any of that
    /// unrelated walk work — pins the width to the group's own read span;
    /// `finish()` (called later, at the same publish point as before)
    /// only decides WHEN that already-decided width becomes visible.
    ///
    /// A guard that never calls this behaves exactly as before:
    /// `finish()` derives the end from elapsed-at-publish-time, which is
    /// what a sampler-stamped group's bracket wants (its member-value
    /// loop runs immediately before `finish()`, so publish-time elapsed
    /// already IS the read span).
    pub(crate) fn mark_end(&mut self) {
        let elapsed = self.begin_mono.elapsed().as_nanos() as u64;
        self.marked_end_ns = Some(self.begin_ns + elapsed);
    }

    /// Stamp begin (captured at `acquire()`) through end into the slot —
    /// the end marked by `mark_end()`, if one was recorded, otherwise
    /// begin plus monotonic elapsed AT THIS CALL (the original behavior).
    /// Call this once the read section succeeded and values are set —
    /// stamp-last, so a racing reader never pairs a value with a window
    /// that outran it.
    pub(crate) fn finish(self) {
        let end_ns = self
            .marked_end_ns
            .unwrap_or_else(|| self.begin_ns + self.begin_mono.elapsed().as_nanos() as u64);
        self.slot.store(Window::new(self.begin_ns, end_ns));
    }

    /// Consume the guard without stamping. Use on an error path to make the
    /// "no update this tick" intent explicit at the call site; a bare
    /// `?`-return that drops the guard has the identical effect.
    ///
    /// No production sampler calls this explicitly today — they all rely on
    /// the identical bare-drop path instead — but it stays part of the
    /// documented API (and is exercised by this module's own test) for
    /// call sites that want the intent spelled out.
    #[allow(dead_code)]
    pub(crate) fn discard(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn member_bound_defaults_unbounded_then_roundtrips() {
        let group = AcquisitionGroup::new("test_sampler", "member_bound_probe");
        assert_eq!(group.member_bound(), None, "unset bound is unbounded");
        group.set_member_bound(3);
        assert_eq!(group.member_bound(), Some(3));
        // Last-write-wins, per the single-init contract documented on
        // `set_member_bound`.
        group.set_member_bound(5);
        assert_eq!(group.member_bound(), Some(5));
    }

    #[test]
    fn new_reader_stamped_is_true_from_construction_with_no_setter_call() {
        // Pins the fix for the gap `set_reader_stamped` alone leaves: a
        // group must read as reader-stamped from the instant its `static`
        // initializes, before any `PackedCounters` ever constructs (or
        // never does, e.g. a disabled sampler, or a snapshot builder test
        // that never runs sampler init at all).
        let group = AcquisitionGroup::new_reader_stamped("test_sampler", "reader_stamped_probe");
        assert!(group.is_reader_stamped());
    }

    #[test]
    fn reader_stamped_defaults_false_then_sticks_true() {
        let group = AcquisitionGroup::new("test_sampler", "reader_stamped_probe");
        assert!(!group.is_reader_stamped(), "default is sampler-stamped");
        group.set_reader_stamped();
        assert!(group.is_reader_stamped());
        // Idempotent: a second call (e.g. two packed_counters() calls
        // sharing one like-entities group) is harmless.
        group.set_reader_stamped();
        assert!(group.is_reader_stamped());
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
    fn mark_end_pins_the_width_to_the_read_span_not_the_publish_delay() {
        // The exact shape this exists for: a group's member-value loop
        // finishes quickly, but `finish()` (publish) is deferred behind
        // unrelated work — e.g. `create_v3`'s emit loop processing every
        // OTHER group before reaching this one. Without `mark_end()`, that
        // deferred-publish delay leaks into the reported width.
        static SLOT: GroupWindowSlot = GroupWindowSlot::new();
        let mut acq = AcquisitionGuard::begin(&SLOT);
        std::thread::sleep(std::time::Duration::from_millis(3)); // the group's own read span
        acq.mark_end();
        std::thread::sleep(std::time::Duration::from_millis(50)); // unrelated walk work, NOT this group's read
        acq.finish();

        let w = SLOT.load().expect("finish() stamped the slot");
        assert!(w.begin_ns > 0, "wall-clock begin");
        assert!(
            w.width_ns() < 20_000_000,
            "width must reflect the ~3ms read span marked by mark_end(), not the ~50ms \
             publish delay after it: {}",
            w.width_ns()
        );
        assert!(
            w.width_ns() >= 2_000_000,
            ">=2ms width (the marked read span itself): {}",
            w.width_ns()
        );
    }

    #[test]
    fn finish_without_mark_end_derives_the_end_at_publish_time_as_before() {
        // A guard that never calls mark_end() behaves exactly as it did
        // before mark_end() existed — the sampler-stamped path, whose
        // member-value loop runs immediately before finish().
        static SLOT: GroupWindowSlot = GroupWindowSlot::new();
        let acq = AcquisitionGuard::begin(&SLOT);
        std::thread::sleep(std::time::Duration::from_millis(3));
        acq.finish();
        let w = SLOT.load().expect("finish() stamped the slot");
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
        // Explicit discard() is documented as identical to the drop path.
        AcquisitionGuard::begin(&SLOT).discard();
        assert!(
            SLOT.load().is_none(),
            "discard() must not stamp, same as the drop path"
        );
    }
}
