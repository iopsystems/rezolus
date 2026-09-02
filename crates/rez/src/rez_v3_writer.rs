//! The `.rez` v3 writer thread. See docs/journal/2026-08-12-rez-sqlite-container.md.
//!
//! Same shape as the v2 writer (`rez_stream.rs`): a dedicated thread behind a
//! bounded channel, so encoding a large segment cannot skew the scrape cadence
//! and a disk that cannot keep up backpressures the loop instead of growing
//! memory — with one bounded exception, unlike v2: a seal batch is encoded
//! whole before its transaction opens, so its segments' bytes are all resident
//! at once (see `seal_batch`). Everything the tar container needed to fake
//! transactions is gone —
//! no `.partial`, no rename, no rename-aside, no checkpoint manifests, no
//! two-sync ordering protocol. **A seal batch is one transaction**, and the
//! file at `path` is a valid, openable `.rez` from the moment `create` returns.
//!
//! Contract: PANIC-FREE — every fallible op returns `Err`. The global panic
//! hook (`src/main.rs`) prints and calls `process::exit(101)` BEFORE
//! unwinding, so a panic here never reaches the send-error path, skips
//! finalize, and in wrapped mode orphans the child.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use metriken_exposition::{GroupSchema, GroupSnapshot, Snapshot};
use tracing::warn;

use super::rez::{dedup_key, entries_approx_bytes, group_approx_bytes, group_by_sampler};
use super::rez_sqlite::{RecordingMeta, RezDb, SegmentMeta, WalRow};
use super::seal_policy::{SealPolicy, SegmentAccount};
use super::wal::{
    encode_wal_group_row, encode_wal_row, materialize_wal_tail, WalCell, WalGroupRow, WalValue,
};

/// Everything known when the recording starts. v3 has no manifest and no
/// per-recording tar directory — a recording IS a row in `recordings` — so the
/// v2 writer's `ManifestSeed` is exactly `RecordingMeta` here, minus `dir`.
/// The name is kept so the two writers read alike at their call sites.
///
/// **`dir` was also a display name, and v3 owes its consumers a substitute.**
/// Besides naming the tar directory it was the user-visible recording name in
/// two places: `parquet metadata`'s `recording {dir} [labels]` line
/// (`src/parquet_tools/metadata.rs`) and the viewer's per-capture display
/// filename (`src/rez_reader.rs` → `capture_registry.rs`). Both should derive
/// one from what v3 stores instead — `rez::recording_dir_slug(&labels)`, which
/// is what produced `dir` in the first place, or `recording {id}` — rather
/// than reintroduce the field. A/B aliasing is NOT affected: both viewer paths
/// alias baseline/experiment on `arm`/`host` labels only, and `labels` survives
/// verbatim in the `recordings` row.
pub type ManifestSeed = RecordingMeta;

enum Msg {
    /// Insert a `recordings` row and hand its id back.
    ///
    /// Goes through the channel rather than being done by the caller because
    /// the writer thread OWNS the connection — the whole design rests on there
    /// being exactly one writing connection, since a second stalls on SQLite's
    /// write lock for `busy_timeout` before failing, which against a tick reads
    /// as a hang. The reply channel is the same shape `Sync` already uses.
    AddRecording {
        seed: Box<ManifestSeed>,
        reply: SyncSender<Result<i64, String>>,
    },
    /// One tick's WAL rows for EVERY recording in the archive, across all
    /// their samplers — one transaction, and therefore one fsync at
    /// `synchronous=FULL`.
    ///
    /// Per tick rather than per recording, because the cost is paid on the
    /// scrape loop: `wal`/`wal_tick` is a blocking send on a bound-1 channel
    /// from inside the tick, and a commit per recording made that cost scale
    /// linearly with endpoint count. `seal_batch` already refused the same
    /// trade ("12 implicit commits would be 12 fsyncs at `synchronous=FULL`
    /// against a ~46 ms tick"); this carries the argument across recordings.
    Wal { ticks: Vec<(i64, Vec<WalRow>)> },
    /// One seal batch for one recording = one transaction.
    Seal {
        recording_id: i64,
        batch: Vec<String>,
    },
    /// Retention: drop everything wholly older than `cutoff_ts`, then trickle
    /// freed pages back if the free list has grown. Only hindsight sends this.
    Evict { recording_id: i64, cutoff_ts: u64 },
    /// One recording's last clock observation; marks *that* recording complete.
    ///
    /// Does NOT stop the writer: an archive may hold several recordings and the
    /// others may still be running. The thread exits when every handle has been
    /// dropped and the channel closes — see `writer_thread`.
    Finalize {
        recording_id: i64,
        clock_offset: (u64, i64),
    },
    /// Stop the writer, whatever else is still holding a sender.
    ///
    /// The exit signal is explicit rather than "the channel closed" because a
    /// handle outliving its archive would otherwise deadlock the join: the
    /// archive drops its own sender and waits, while the handle's clone keeps
    /// the channel open forever. With this, a leaked handle merely finds the
    /// receiver gone on its next send — the failure path it already has.
    Shutdown,
    /// Reply once everything queued ahead of this has been committed. Carries
    /// no data and changes nothing — see [`RecordingWriter::sync`].
    #[cfg(any(test, feature = "test-support"))]
    Sync(SyncSender<()>),
    /// Answer with how many transactions the writer's connection has
    /// committed. A barrier as well as a question, exactly as `Sync` is: the
    /// reply means everything queued ahead of it has been handled, so a caller
    /// can count a tick's commits without racing the writer.
    #[cfg(any(test, feature = "test-support"))]
    Commits(SyncSender<u64>),
}

/// Where the writer thread leaves its failure so a *handle* can report it.
///
/// With one recording per archive the handle owned the thread, so a send
/// failure could join and surface the real error. An archive with several
/// recordings has one thread and many handles, and a handle cannot join what it
/// does not own — so the thread stores its error here on the way out and every
/// handle reads it, keeping per-tick errors as specific as they were.
type ErrorSlot = Arc<Mutex<Option<String>>>;

/// Reclaim at most this many pages per retention pass — sized to fit inside a
/// tick. The point of a cap at all is that a shrunken working set drains back
/// to the filesystem gradually; a full `VACUUM` would return the same space in
/// one step and stall the recording for seconds doing it.
const RECLAIM_PAGES_PER_PASS: u32 = 100;

/// Reclaim only once the free list exceeds this fraction of the file, as a
/// divisor: `freelist_count * RECLAIM_FREELIST_DIVISOR > page_count`.
///
/// Steady-state eviction reuses freed pages, so the free list stays a rounding
/// error on a healthy rolling buffer and never pays for a reclaim it does not
/// need. This fires only when the working set genuinely shrank and left the
/// file many times larger than its contents, which is the one situation where
/// handing pages back is worth anything.
const RECLAIM_FREELIST_DIVISOR: u32 = 10;

/// Handle to the writer thread. Every fallible hand-off reports the writer's
/// stored error, in the required order: send-failure → join → report.
pub struct RezArchive {
    /// The master sender. Kept only to clone per-recording handles from, and
    /// dropped by `join` so the writer's channel can actually close.
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
    err: ErrorSlot,
}

impl RezArchive {
    /// Create the `.rez` at `path` and spawn its writer thread.
    ///
    /// The file is a valid, openable `.rez` from the moment this returns:
    /// there is no `.partial`, no rename at the end, and nothing to move
    /// aside at the start (`RezDb::create` refuses an existing file
    /// atomically). That property is what retires the whole staging dance —
    /// an early-killed recording is just a recording whose `complete` is 0.
    ///
    /// The archive holds no recordings yet; add each with `add_recording`.
    pub fn create(path: &Path) -> Result<Self, String> {
        Self::create_checkpointing_every(path, CHECKPOINT_INTERVAL)
    }

    /// [`create`](Self::create) with the WAL checkpoint cadence chosen by the
    /// caller.
    ///
    /// Exists so the staleness bound is testable: asserting it through
    /// `create` would mean a test that sleeps [`CHECKPOINT_INTERVAL`].
    /// Production takes the constant.
    pub fn create_checkpointing_every(
        path: &Path,
        checkpoint_every: Duration,
    ) -> Result<Self, String> {
        let db = RezDb::create(path)?;

        // Bound 1, as in v2: the hand-off blocks while the writer is busy,
        // which is the intended backpressure signal. One slot for the archive
        // rather than per recording, deliberately — the writer is a single
        // thread against a single write lock, so a deeper queue would only
        // move the wait, and one recording falling behind SHOULD apply
        // backpressure to the shared scrape loop rather than growing a buffer.
        let (tx, rx) = sync_channel(1);
        let err: ErrorSlot = Arc::new(Mutex::new(None));
        let thread_err = Arc::clone(&err);
        // A spawn failure leaves the file in place, unlike v2's `.partial`
        // unlink: it is a valid empty recording at the caller's chosen output
        // path, not a staging artifact a later run would have to interpret.
        let thread = std::thread::Builder::new()
            .name("rez-v3-writer".to_string())
            .spawn(move || writer_thread(rx, db, thread_err, checkpoint_every))
            .map_err(|e| format!("failed to spawn the .rez writer thread: {e}"))?;

        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            path: path.to_path_buf(),
            err,
        })
    }

    /// Open one recording in this archive and return its writer handle.
    ///
    /// Several may be open at once — that is the point of the container's
    /// label-tagged `recordings` list — and they are independent: each has its
    /// own segment sequences, its own clock-offset series, and its own
    /// `complete` flag.
    pub fn add_recording(&mut self, seed: ManifestSeed) -> Result<RecordingWriter, String> {
        // Derived before the seed is sent, since the seed moves.
        let stagger_key = crate::seal_policy::recording_stagger_key(&seed.labels);
        let Some(tx) = self.tx.as_ref() else {
            return Err("the .rez writer thread has already been joined".to_string());
        };
        let (reply_tx, reply_rx) = sync_channel(0);
        if tx
            .send(Msg::AddRecording {
                seed: Box::new(seed),
                reply: reply_tx,
            })
            .is_err()
        {
            return Err(self.take_error());
        }
        let recording_id = match reply_rx.recv() {
            Ok(inserted) => inserted?,
            // The writer died between accepting the message and replying.
            Err(_) => return Err(self.take_error()),
        };
        Ok(RecordingWriter {
            tx: tx.clone(),
            recording_id,
            stagger_key,
            err: Arc::clone(&self.err),
            path: self.path.clone(),
        })
    }

    /// The archive being written — valid and readable while it is written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close the channel and join the writer, returning its stored result.
    /// Idempotent: a second call is a no-op `Ok`.
    ///
    /// Handles should be dropped first — but not because this would otherwise
    /// block. `Shutdown` is sent below *before* our own sender is released, and
    /// the writer honours it whoever else still holds a clone, so a wrong order
    /// is an error (work queued after the stop is dropped), not a hang. That
    /// distinction is load-bearing: the guarantee lives in `Msg::Shutdown`, not
    /// in the drop order, and removing it would turn every "must drop first"
    /// note in this file into a real deadlock.
    pub fn join(&mut self) -> Result<(), String> {
        // Tell the writer to stop before releasing our own sender. A handle
        // that outlived its archive still holds a clone, so waiting for the
        // channel to close on its own could wait forever; `Shutdown` ends the
        // loop regardless of who is still holding one. A failed send just
        // means the writer already exited.
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(Msg::Shutdown);
        }
        self.tx = None;
        match self.thread.take() {
            // The panic arm is unreachable by contract (the global hook exits
            // the process before unwinding); it exists so this path cannot
            // itself panic.
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err("the .rez writer thread panicked".to_string())),
            None => Ok(()),
        }
    }

    fn take_error(&mut self) -> String {
        take_writer_error(&self.err)
    }

    /// Commit one tick's staged rows for EVERY recording, as one transaction.
    ///
    /// The multi-recording counterpart to [`RecordingWriter::wal`]. Each
    /// recording's rows come from [`StreamRecorderV3::stage`]; this hands them
    /// over together so the archive pays one commit — one fsync at
    /// `synchronous=FULL` — per tick rather than one per endpoint.
    ///
    /// **Why the cost is worth naming:** the hand-off is a blocking send on a
    /// bound-1 channel from inside the scrape tick, so a per-recording commit
    /// put a linear-in-endpoint-count fsync bill on the loop that has to keep
    /// up with the sampling interval. `seal_batch` already refused exactly this
    /// trade within one recording; this is the same argument across them.
    ///
    /// An empty batch does not send: it still checks the writer is alive, so a
    /// tick where nothing advanced cannot mask a dead writer.
    pub fn wal_tick(&mut self, ticks: Vec<(i64, Vec<WalRow>)>) -> Result<(), String> {
        let ticks: Vec<(i64, Vec<WalRow>)> = ticks
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .collect();
        if ticks.is_empty() {
            return self.check_alive();
        }
        let Some(tx) = self.tx.as_ref() else {
            return Err("the .rez writer thread has already been joined".to_string());
        };
        if tx.send(Msg::Wal { ticks }).is_ok() {
            return Ok(());
        }
        Err(take_writer_error(&self.err))
    }

    /// How many transactions the writer has committed, as a barrier: the
    /// answer arrives only after everything queued ahead of it is handled.
    ///
    /// Exists so "one commit per tick, whatever the endpoint count" is a
    /// property a test asserts rather than a comment claims — an fsync is not
    /// observable from inside the process, but the commit that causes it is.
    #[cfg(any(test, feature = "test-support"))]
    pub fn commits_for_test(&mut self) -> u64 {
        let (tx, rx) = sync_channel(0);
        let Some(sender) = self.tx.as_ref() else {
            return 0;
        };
        if sender.send(Msg::Commits(tx)).is_err() {
            return 0;
        }
        rx.recv().unwrap_or(0)
    }

    /// Whether the writer thread is still alive, without writing anything.
    ///
    /// Mirrors `RecordingWriter::check_alive`: the shared error slot is the
    /// only signal available, since the archive cannot ask a thread it owns
    /// whether it has finished without joining it.
    fn check_alive(&mut self) -> Result<(), String> {
        match self.err.lock() {
            Ok(guard) if guard.is_some() => Err(guard.clone().unwrap_or_default()),
            _ => Ok(()),
        }
    }

    /// Create an archive holding exactly one recording.
    ///
    /// The shape every caller had before archives could hold several, and
    /// still what hindsight and a single-endpoint `record` run want. Returns
    /// both halves because the archive owns the writer thread and must outlive
    /// the handle — `Shutdown` means a wrong order is an error rather than a
    /// hang, but the right order is still: finish with the handle, then join.
    /// Finalize the one recording and join the writer, so the file is fully
    /// committed when this returns.
    ///
    /// The synchronous shape callers had before `finalize` was split: the
    /// handle can only *queue* completion now, since the archive owns the
    /// thread, so anything that reads the file straight afterwards has to join
    /// too. Mirrors `RezStream::finalize` in the recorder.
    #[cfg(test)]
    pub fn finalize_single(
        mut self,
        writer: RecordingWriter,
        clock_offset: (u64, i64),
    ) -> Result<(), String> {
        let queued = writer.finalize(clock_offset);
        let joined = self.join();
        queued.and(joined)
    }

    /// As `finalize_single`, but for a caller holding the `StreamRecorderV3`
    /// (which owns the handle) rather than a bare `RecordingWriter`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn finalize_single_rec(
        mut self,
        rec: StreamRecorderV3,
        clock_offset: (u64, i64),
    ) -> Result<(), String> {
        let queued = rec.finalize(clock_offset);
        let joined = self.join();
        queued.and(joined)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn single(path: &Path, seed: ManifestSeed) -> Result<(Self, RecordingWriter), String> {
        let mut archive = Self::create(path)?;
        let writer = archive.add_recording(seed)?;
        Ok((archive, writer))
    }
}

impl Drop for RezArchive {
    /// The writer must be joined on every path out — including the ones that
    /// skip an explicit join — so a dropped archive never leaves a detached
    /// thread still writing to the database.
    fn drop(&mut self) {
        if let Err(e) = self.join() {
            warn!("the .rez writer failed: {e}");
        }
    }
}

/// One recording's handle onto a shared archive writer.
///
/// Cheap and cloneable-in-spirit: it is a sender plus an id. Dropping it
/// releases this recording's claim on the writer; the thread exits once every
/// handle *and* the archive's master sender are gone.
pub struct RecordingWriter {
    tx: SyncSender<Msg>,
    recording_id: i64,
    /// This recording's stagger identity — its canonical label set. Held here
    /// so the seal policy can desync tables ACROSS recordings as well as
    /// within one; see `stagger_bucket`.
    stagger_key: String,
    err: ErrorSlot,
    /// The archive this recording lives in. Carried per handle so a caller
    /// holding only a recording can still name its file — one `PathBuf` per
    /// recording, against an archive that holds at most a handful.
    path: PathBuf,
}

impl RecordingWriter {
    /// The archive being written — valid and readable while it is written.
    ///
    /// Reachable only through `StreamRecorderV3::path`, which no live caller
    /// uses: the recorder asks the archive directly. Kept because a recorder
    /// naming its own output is the obvious thing to want.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `recordings` row this handle appends to.
    pub fn recording_id(&self) -> i64 {
        self.recording_id
    }

    #[cfg_attr(not(test), allow(dead_code))]
    /// This recording's stagger identity — see `stagger_bucket`.
    pub fn stagger_key(&self) -> &str {
        &self.stagger_key
    }

    /// Hand one tick's WAL rows to the writer, for THIS recording alone.
    ///
    /// The single-recording spelling — hindsight, and a one-endpoint `record`.
    /// An archive with several recordings should stage each one and commit the
    /// tick once, through [`RezArchive::wal_tick`]: one transaction instead of
    /// one per recording.
    pub fn wal(&mut self, rows: Vec<WalRow>) -> Result<(), String> {
        if rows.is_empty() {
            return self.check_alive();
        }
        self.send(Msg::Wal {
            ticks: vec![(self.recording_id, rows)],
        })
    }

    /// Hand one seal batch (= one transaction) to the writer, as the samplers
    /// to seal. Blocks while the channel is full: that is the intended
    /// backpressure signal.
    pub fn seal(&mut self, batch: Vec<String>) -> Result<(), String> {
        if batch.is_empty() {
            return self.check_alive();
        }
        self.send(Msg::Seal {
            recording_id: self.recording_id,
            batch,
        })
    }

    /// Ask the writer to apply retention at `cutoff_ts`.
    ///
    /// It goes through the writer thread rather than a second connection for
    /// the same reason everything else does: the writer OWNS this file, and a
    /// second writing connection would stall on the write lock for up to
    /// `busy_timeout` (5 s, rusqlite's default) before failing — which against
    /// a tick reads as a hang. Readers are unaffected either way; WAL mode
    /// lets them proceed while this commits.
    ///
    /// Fire-and-forget, like `wal` and `seal`: a failure surfaces on the next
    /// hand-off, which is the convention the whole writer follows.
    pub fn evict_before(&mut self, cutoff_ts: u64) -> Result<(), String> {
        self.send(Msg::Evict {
            recording_id: self.recording_id,
            cutoff_ts,
        })
    }

    /// Block until everything handed off so far has been committed.
    ///
    /// **The one place the writer is not fire-and-forget, and it exists because
    /// the file lags the caller.** Every other hand-off queues work and returns
    /// immediately, so a caller that hands off an ingest or an eviction and
    /// then opens a SECOND connection to look at the file — `summarize`, a
    /// dump, `/status` — can legitimately observe the state from before its own
    /// last call. That is fine for a status reading and fatal for an assertion.
    ///
    /// Ordering is what makes this work rather than any locking: the channel is
    /// FIFO and the writer is single-threaded, so the reply cannot be sent
    /// until every earlier message has been fully handled. With several
    /// recordings sharing one writer that is *stronger* than it was, not
    /// weaker: the barrier covers the other recordings' queued work too.
    ///
    /// A dropped reply channel is treated as success — it means the writer
    /// exited, and its error surfaces through the usual hand-off path rather
    /// than here.
    ///
    /// **Test-only, and that is a statement about the callers rather than the
    /// mechanism.** Nothing in production asserts on the file immediately after
    /// handing off a tick: `/status` reporting retention a tick behind is
    /// inherent to an asynchronous writer and harmless. Tests do assert it, and
    /// without a barrier they race the writer. Give this a `cfg`-free home the
    /// moment a real caller needs to see its own last tick.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sync(&mut self) -> Result<(), String> {
        let (tx, rx) = sync_channel(0);
        self.send(Msg::Sync(tx))?;
        let _ = rx.recv();
        Ok(())
    }

    /// Record this recording's final clock offset and mark it complete.
    ///
    /// Consumes the handle, which is what releases its sender: the writer
    /// thread ends when the last handle and the archive's master sender are
    /// gone, so a handle kept alive past its finalize would stall the join.
    pub fn finalize(mut self, clock_offset: (u64, i64)) -> Result<(), String> {
        self.send(Msg::Finalize {
            recording_id: self.recording_id,
            clock_offset,
        })
    }

    /// Report a writer that has already failed, on a hand-off that sends
    /// nothing. Without it, writer health would only be polled when there is
    /// something to write, and a recording whose writer died would go on
    /// reporting success for every empty tick in between.
    fn check_alive(&mut self) -> Result<(), String> {
        // The shared error slot is the only signal available here: the thread
        // belongs to the archive, so this cannot ask whether it has finished,
        // and it deliberately does not send — a probe message would be a write
        // on a path whose whole point is that it has nothing to write. A
        // writer that exited *cleanly* while this handle is live is therefore
        // invisible here, which cannot happen today because the only clean
        // exit is `Shutdown`, sent last.
        match self.err.lock() {
            Ok(guard) if guard.is_some() => Err(guard.clone().unwrap_or_default()),
            _ => Ok(()),
        }
    }

    fn send(&mut self, msg: Msg) -> Result<(), String> {
        if self.tx.send(msg).is_ok() {
            return Ok(());
        }
        // The receiver is gone, so the writer has exited (it exits its receive
        // loop on the first error). The thread stored its error on the way out
        // — see `ErrorSlot` — so report that rather than logging per-tick
        // against a broken recording.
        Err(take_writer_error(&self.err))
    }
}

/// Read the writer thread's stored failure, or a generic one if it exited
/// without recording anything (a clean exit that a handle nonetheless outlived).
fn take_writer_error(slot: &ErrorSlot) -> String {
    slot.lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| {
            "the .rez writer thread exited before the recording finished".to_string()
        })
}

/// An encoded segment waiting to be inserted.
struct Encoded {
    sampler: String,
    seq: u64,
    meta: SegmentMeta,
    bytes: Vec<u8>,
}

/// The writer thread body. Every fallible operation returns `Err`; the loop
/// exits on the first error so the failure surfaces on the next hand-off
/// instead of accumulating against a broken recording.
fn writer_thread(
    rx: Receiver<Msg>,
    mut db: RezDb,
    err_slot: ErrorSlot,
    checkpoint_every: Duration,
) -> Result<(), String> {
    // `rx` is BORROWED by the loop, not moved into it, so the receiver outlives
    // the error store below. That ordering is the whole point: a handle's send
    // fails the instant the receiver drops, and if the slot were still empty at
    // that moment the handle would report a generic "writer exited" instead of
    // the writer's own error. Holding `rx` here means the channel is still open
    // while the slot is written, so any send that fails afterwards finds it.
    let result = writer_loop(&rx, &mut db, checkpoint_every);
    if let Err(ref e) = result {
        *err_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
    }
    result
}

/// How stale a plain copy of a live archive is allowed to be.
///
/// SQLite commits into a `<file>-wal` sidecar and folds it into the archive at
/// a checkpoint, so a copy of the archive ALONE — which is what anyone who
/// `cp`s one, or uploads one to a browser, ends up with — is a consistent view
/// as of the last checkpoint and nothing after it. That copy is not corrupt; it
/// simply ends early, and nothing about it says so.
///
/// [`crate::rez_sqlite`]'s autocheckpoint bounds how many BYTES can accumulate
/// (4 MiB). It cannot bound how much TIME they represent: a busy recording
/// crosses 4 MiB in seconds, a quiet one in hours, and the quiet one is the
/// case where a copy is silently useless. Measured before this existed: 123
/// ticks — about two minutes at a 1s interval — missing from a plain copy of a
/// 2000-tick recording.
///
/// 10s is chosen to be short against the window anyone reasons about (an
/// incident, a benchmark run) and long against the work: a passive checkpoint
/// of one interval's frames is a few tens of KiB at a typical fleet cadence,
/// and it runs on the writer THREAD rather than the scrape loop. It does not
/// make a copy exact — `rezolus recording snapshot` does that — it makes what a
/// copy loses bounded and small.
pub const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(10);

fn writer_loop(
    rx: &Receiver<Msg>,
    db: &mut RezDb,
    checkpoint_every: Duration,
) -> Result<(), String> {
    // Next segment sequence number, per (recording, sampler). Keyed by both
    // because `seq` is scoped to a recording's sampler in the `segments` table:
    // two recordings of the same host have the same sampler names and each
    // needs its own sequence.
    let mut next_seq: BTreeMap<(i64, String), u64> = BTreeMap::new();
    // Timestamps each recording's `clock_offsets` series already carries. Only
    // finalize reads it, but it has to be maintained as batches seal.
    let mut observed: BTreeMap<i64, BTreeSet<u64>> = BTreeMap::new();
    // How many recordings were opened, and how many closed cleanly. Reclaim at
    // exit only when they match: an unclean exit is the recovery artifact and
    // must not pay for a vacuum on the way down.
    let mut added: usize = 0;
    let mut finalized: usize = 0;
    // When the sidecar was last folded into the archive. Advanced on every
    // checkpoint, including ones taken while idle — the guarantee is about
    // elapsed time, not about arriving messages.
    let mut last_checkpoint = Instant::now();

    loop {
        // `recv_timeout`, not `recv`: a writer with nothing to do still has to
        // wake and checkpoint. A recording that has gone quiet is exactly when
        // someone copies it.
        let waited = rx.recv_timeout(checkpoint_every.saturating_sub(last_checkpoint.elapsed()));
        if last_checkpoint.elapsed() >= checkpoint_every {
            // Best-effort: a checkpoint that cannot proceed (a reader is
            // holding an older snapshot) is not an error, and failing the
            // writer over one would trade every subsequent tick for a copy's
            // freshness.
            if let Err(e) = db.checkpoint_passive() {
                warn!("failed to checkpoint the WAL: {e}");
            }
            last_checkpoint = Instant::now();
        }
        let received = match waited {
            Ok(msg) => Ok(msg),
            // Nothing arrived within the checkpoint window: go round again.
            Err(RecvTimeoutError::Timeout) => continue,
            // Every handle is gone. Falls into the same arm the blocking
            // `recv` used to reach.
            Err(RecvTimeoutError::Disconnected) => Err(()),
        };
        match received {
            // Nothing to do but answer: arriving here at all means every
            // message queued before it has already been handled.
            #[cfg(any(test, feature = "test-support"))]
            Ok(Msg::Sync(reply)) => {
                let _ = reply.send(());
            }
            Ok(Msg::AddRecording { seed, reply }) => {
                let inserted = db.insert_recording(&seed);
                // A failed insert is reported to the caller and does NOT kill
                // the writer: an archive's other recordings are still valid,
                // and the caller decides whether to give up.
                if inserted.is_ok() {
                    added += 1;
                }
                let _ = reply.send(inserted);
            }
            Ok(Msg::Wal { ticks }) => db.insert_wal_rows_batch(&ticks)?,
            #[cfg(any(test, feature = "test-support"))]
            Ok(Msg::Commits(reply)) => {
                let _ = reply.send(db.commits());
            }
            Ok(Msg::Seal {
                recording_id,
                batch,
            }) => {
                if let Some(ts) = seal_batch(db, recording_id, &mut next_seq, batch)? {
                    observed.entry(recording_id).or_default().insert(ts);
                }
            }
            Ok(Msg::Evict {
                recording_id,
                cutoff_ts,
            }) => {
                db.evict_before(recording_id, cutoff_ts)?;
                reclaim_if_fragmented(db)?;
            }
            Ok(Msg::Finalize {
                recording_id,
                clock_offset,
            }) => {
                // The loop's final tick observation joins the series only when
                // it adds a timestamp no sealed row already covers — otherwise
                // the series would carry two conflicting offsets at one
                // timestamp and consumers could not read it uniformly. The
                // row-derived value wins because it is a projection of the
                // `:wall_offset` column the segment itself carries. Same rule,
                // and same reason, as the v2 writer.
                let novel = !observed
                    .get(&recording_id)
                    .is_some_and(|o| o.contains(&clock_offset.0));
                db.transaction(|tx| {
                    if novel {
                        tx.insert_clock_offset(recording_id, clock_offset.0, clock_offset.1)?;
                    }
                    tx.mark_complete(recording_id)
                })?;
                finalized += 1;
                // Deliberately NOT returning here, and not reclaiming yet. An
                // archive may hold several recordings; this one is complete,
                // the others may still be writing. The reclaim is a
                // whole-file operation and belongs at the end, once — see the
                // loop's exit below.
            }
            // Asked to stop. Same accounting as the channel-close arm below:
            // reclaim only if every recording opened was also finalized.
            Ok(Msg::Shutdown) => {
                if added > 0 && finalized == added {
                    reclaim_all(db)?;
                }
                return Ok(());
            }
            // Every handle has been dropped, so no further work can arrive.
            //
            // If all the recordings that were opened also finalized, this is a
            // clean close and the free list is drained once, here — the place
            // the single-recording writer did it inside its `Finalize` arm.
            // AFTER every `mark_complete`, deliberately: reclaiming space is an
            // optimization, and a crash partway through it must leave complete
            // recordings that are merely larger than they needed to be, never
            // incomplete ones that happen to be compact.
            //
            // Otherwise a handle was dropped without finalizing. Nothing to
            // clean up and nothing to reclaim: the file is already a valid
            // `.rez` holding every committed tick, with `complete` still 0 —
            // that is the recovery artifact, and a shutdown that may be a kill
            // must not pay for a vacuum on the way down.
            Err(_) => {
                if added > 0 && finalized == added {
                    reclaim_all(db)?;
                }
                return Ok(());
            }
        }
    }
}

/// Hand freed pages back to the filesystem, but only once the free list is a
/// noticeable fraction of the file. See the two constants for why the guard is
/// there: without it this would run every pass for no gain, and without the
/// reclaim a buffer that shrank would keep its high-water size forever.
fn reclaim_if_fragmented(db: &RezDb) -> Result<(), String> {
    if should_reclaim(
        db.pragma_u32("freelist_count")?,
        db.pragma_u32("page_count")?,
    ) {
        db.incremental_vacuum(RECLAIM_PAGES_PER_PASS)?;
    }
    Ok(())
}

/// The guard, as a decision rather than a branch — because it is a decision
/// about COST, not about outcome: reclaiming an unfragmented file is a no-op
/// either way, so the only way to test the threshold is to ask it directly.
fn should_reclaim(free_pages: u32, pages: u32) -> bool {
    free_pages.saturating_mul(RECLAIM_FREELIST_DIVISOR) > pages
}

/// Drain the whole free list back to the filesystem, in one go. Finalize only.
///
/// **Without this a finished recording keeps every page its WAL pruning freed.**
/// Pruning deletes rows continuously — that is how the WAL stays a tail rather
/// than a second copy of the recording — and each deleted row's page lands on
/// SQLite's free list, available for reuse but never returned to the
/// filesystem. `reclaim_if_fragmented` is the trickle that returns them, but
/// only the retention path calls it, so a `record` run reclaims nothing. The
/// sparser the recording, the larger the share of the file that is dead.
///
/// Unguarded, unlike the retention path. `should_reclaim` exists to keep a
/// *recurring* per-tick cost off a file that would not benefit; this runs once,
/// at the end, on a file nobody is waiting to write to again, and on an already
/// compact file it is a no-op costing one `freelist_count` lookup.
///
/// Uncapped, also unlike the retention path: `RECLAIM_PAGES_PER_PASS` bounds a
/// pass so a reclaim cannot overrun a tick, and there is no next tick here.
/// `u32::MAX` is "as many as the free list holds" — `incremental_vacuum` stops
/// when it runs out.
fn reclaim_all(db: &RezDb) -> Result<(), String> {
    db.incremental_vacuum(u32::MAX)
}

/// Encode one batch's segments, insert them — with the batch's clock
/// observation — in ONE transaction, then prune the sealed samplers' WAL
/// outside it. Returns the timestamp of the observation recorded, if any.
fn seal_batch(
    db: &mut RezDb,
    recording_id: i64,
    next_seq: &mut BTreeMap<(i64, String), u64>,
    batch: Vec<String>,
) -> Result<Option<u64>, String> {
    // Read and encode BEFORE the transaction opens. Both are proportional to
    // segment size and would hold the write lock for their whole duration.
    //
    // `live_wal` is what defines the segment: its watermark returns exactly the
    // rows past this sampler's newest sealed segment, and because the ingest
    // side hands rows and seal requests down one FIFO channel, those are
    // exactly the rows the seal decision was made about. Nothing has to be
    // snapshotted or passed along for that to hold.
    let mut encoded = Vec::with_capacity(batch.len());
    // The batch's clock observation: the NEWEST sealed row's
    // `(timestamp, wall_offset)`, paired with that same table's offset — never
    // one table's timestamp against another's. Derived from the rows just
    // sealed, so every entry in the series is a projection of the
    // `:wall_offset` column it summarizes, exactly as in v2.
    let mut observation: Option<(u64, i64)> = None;
    for sampler in batch {
        let rows = db.live_wal(recording_id, &sampler)?;
        let Some(last) = rows.last() else {
            // No live rows: nothing to catalog and nothing to prune. The ingest
            // side never seals an empty segment, and a sampler whose rows were
            // already sealed is not an error worth failing the recording over.
            continue;
        };
        // `last_ts`/`wall_offset`: always the raw WAL span's own last row,
        // for BOTH containers — a V3 group's un-anchored skip (see
        // `materialize_group_wal_tail`) is always a LEADING run (retention
        // removes a prefix, never punches a hole), so the last row is never
        // itself skipped. `first_ts`/`rows` are NOT this simple — see below.
        let (last_ts, wall_offset) = (last.ts, last.wall_offset);
        let Some(tail) = materialize_wal_tail(&sampler, &rows)
            .map_err(|e| format!("failed to encode a {sampler} segment: {e}"))?
        else {
            continue;
        };
        // `>=`, so a later sampler wins a tie — same rule as v2's
        // `seal_segments`.
        if observation.is_none_or(|(seen, _)| last_ts >= seen) {
            observation = Some((last_ts, wall_offset));
        }
        // Bumped before the commit, which is safe only because the writer
        // exits on its first error: no later batch ever reuses this map.
        // Keyed by recording as well as sampler: `segments.seq` is scoped to
        // `(recording_id, sampler)`, so two recordings of the same host must
        // not share a counter.
        let seq = next_seq.entry((recording_id, sampler.clone())).or_insert(0);
        encoded.push(Encoded {
            sampler,
            seq: *seq,
            meta: SegmentMeta {
                // From `tail`, NOT `rows.len()`/`rows.first().ts`: a V3
                // group table's leading un-anchored rows (skipped, see
                // `materialize_group_wal_tail`) are real WAL rows but never
                // reach the segment, so the raw WAL span would catalog a row
                // count and start the catalog does not agree with the bytes
                // being inserted — display-only (`parquet metadata`,
                // hindsight `/status`, `cadence_ns`), but still wrong data.
                // `materialize_sampler_wal_tail` never skips, so for a V1/V2
                // table `tail.rows`/`tail.first_ts` are simply `rows.len()`/
                // `rows.first().ts` again — this is a no-op there.
                rows: tail.rows,
                first_ts: tail.first_ts,
                last_ts,
            },
            bytes: tail.bytes,
        });
        *seq += 1;
    }

    // ONE transaction for the whole batch. The fleet seals 12 tables in
    // lockstep, and 12 implicit commits would be 12 fsyncs at
    // `synchronous=FULL` against a ~46 ms tick.
    //
    // The batch's clock observation rides along inside it, for free: no extra
    // commit, no extra fsync, and it lands iff the segments it was derived
    // from do. It is a `clock_offsets` ROW rather than something a reader has
    // to dig out of a segment, which is what keeps drift readable from the
    // catalog alone — including on a recording that is killed before it ever
    // finalizes, where these are the only observations there will be.
    db.transaction(|tx| {
        for e in &encoded {
            tx.insert_segment(recording_id, &e.sampler, e.seq, &e.meta, &e.bytes)?;
        }
        if let Some((ts, offset)) = observation {
            tx.insert_clock_offset(recording_id, ts, offset)?;
        }
        Ok(())
    })?;

    // OUTSIDE the transaction, deliberately: a quiet sampler accumulates
    // thousands of rows before it seals, so pruning inside the seal commit puts
    // a large delete on the tick path. `live_wal`'s watermark filter makes a
    // crash between the commit above and the delete below harmless — a
    // straddling row is simply not live — which leaves the prune a pure
    // background optimisation. `RezTx` does not expose `prune_wal`, so this
    // ordering is enforced by the type, not by this comment.
    //
    // Each sampler is pruned only up to its OWN segment's `last_ts`: rows a
    // sampler ingested after the sealed span, and every other sampler's rows,
    // stay live.
    for e in &encoded {
        db.prune_wal(recording_id, &e.sampler, e.meta.last_ts)?;
    }
    Ok(observation.map(|(ts, _)| ts))
}

/// The scrape-side half of the v3 writer: per-sampler open segments, the seal
/// decision, and — new in v3 — a WAL row per sampler per tick.
///
/// Everything except that last part is v2's `StreamRecorder` verbatim, by
/// *importing* its pieces rather than copying them: `SealPolicy`,
/// `BuilderState` (with `open_first`'s FNV-1a stagger and `seal_completed`'s
/// rotation), `TableBuilder`, `group_by_sampler` and `dedup_key`. A second copy
/// of the dedup rule is exactly how the two containers would drift.
///
/// The per-tick WAL write is the entire point of the container swap. In v2,
/// kill-safety was per-segment: at `kill -9` 120 s into a fleet recording, 16
/// of 26 tables recovered nothing at all because they had not sealed yet. Here
/// every tick is committed as it arrives, for quiet tables as much as busy
/// ones, so an unclean kill costs one tick rather than a whole open segment.
pub struct StreamRecorderV3 {
    /// Open segment per sampler — the seal decision's inputs, and no rows.
    ///
    /// **The rows live in the WAL and nowhere else.** v2 keeps a parallel
    /// `TableBuilder` per sampler and encodes it at seal time; here the WAL row
    /// committed each tick is already a complete record of that tick, so
    /// buffering the same values a second time would double both the per-tick
    /// allocation and the resident footprint to hold a copy that has to agree
    /// with the WAL about what a segment contains. Sealing reads them back
    /// instead — see `maybe_seal`.
    accounts: BTreeMap<String, SegmentAccount>,
    /// Window-advance dedup keys. Held here, not on the account, so dedup
    /// survives a segment rotation: the key of a row in an already-sealed
    /// segment must still suppress a re-observation.
    last_keys: BTreeMap<String, u64>,
    /// Per sampler, the metrics whose metadata is already in the CURRENT
    /// segment's WAL span. Cleared for a sampler when it seals, so each segment
    /// re-anchors its own metadata — see `WalCell::metadata`.
    described: BTreeMap<String, HashSet<String>>,
    /// V3 group schema cache: `group_name -> ring of its last few
    /// (schema_hash, schema)` generations, for the life of the recording (NOT
    /// reset on rotation — see `segment_schema` for the per-segment concern).
    /// Resolves a `schema: None` payload (the agent's own cache-hit) and
    /// dedups a repeated `schema: Some` payload. Only ever gets an entry
    /// AFTER `GroupSnapshot::validate()` passes — never cache a schema that
    /// failed validation.
    ///
    /// **Bounded to [`SCHEMA_RING_LEN`] entries per name, not unbounded.**
    /// A `HashMap<(name, hash), schema>` that never evicts re-introduces
    /// exactly the unbounded growth V1/V2's `described` set was fixed to
    /// avoid — a hash churns on every membership change (a cgroup added or
    /// removed), each generation retaining a full `Vec<MetricDesc>` with a
    /// per-metric `BTreeMap`, in the always-on hindsight process, with a
    /// heavier payload than `described` ever carried. A ring is sufficient
    /// because the AGENT'S own schema cache holds exactly ONE entry per group
    /// name (metriken-exposition's producer contract), so a `schema: None`
    /// payload can only ever reference the newest hash for that name — this
    /// only needs slack for an in-flight transition (a schema change
    /// straddling one scrape), not deep history.
    schemas: HashMap<String, SchemaRing>,
    /// Per V3 group, the schema hash already embedded in a WAL row for the
    /// CURRENT segment's live WAL span. Cleared for a table when it seals
    /// (alongside `described`), so the next row after rotation re-anchors —
    /// this is what keeps a group's WAL rows self-sufficient for
    /// `materialize_wal_tail` (see `WalGroupRow`), independent of whether the
    /// AGENT chose to resend its schema this tick.
    segment_schema: BTreeMap<String, (u64, u64)>,
    /// Rate-limits the warnings `ingest_v3` logs for a given group + failure
    /// reason (validation failure, unknown schema, arity mismatch) to once
    /// each, keyed by an arbitrary distinguishing string built from the group
    /// name and the reason.
    warned: HashSet<String>,
    /// Schema-hash cache hit/miss counts, exposed for tests.
    schema_stats: SchemaCacheStats,
    handle: RecordingWriter,
    policy: SealPolicy,
}

/// V3 schema-hash cache hit/miss counters — see `StreamRecorderV3::schemas`.
/// A **hit** is a tick resolved without learning a new schema (the hash was
/// already cached, whether because the payload repeated it or omitted it and
/// relied on the cache). A **miss** is a tick that taught the cache a schema
/// it had not seen before for that group name. A tick that fails validation,
/// or references an unknown hash with no schema attached, affects neither —
/// it is an error, not a cache event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaCacheStats {
    pub hits: u64,
    pub misses: u64,
}

/// Cap on cached schema generations kept PER GROUP NAME in
/// `StreamRecorderV3::schemas`. The agent's own schema cache holds exactly
/// one entry per group name, so a `schema: None` payload can only ever
/// reference the newest hash for that name — 3 is generous headroom over the
/// 1 the agent itself needs, enough slack for a schema change straddling one
/// scrape without letting the cache grow with cgroup/task churn.
const SCHEMA_RING_LEN: usize = 3;

/// One group name's cached schema generations, oldest first — see
/// `StreamRecorderV3::schemas` and [`SCHEMA_RING_LEN`].
type SchemaRing = std::collections::VecDeque<((u64, u64), Arc<GroupSchema>)>;

impl StreamRecorderV3 {
    pub fn new(handle: RecordingWriter) -> Self {
        Self::with_policy(handle, SealPolicy::default())
    }

    pub fn with_policy(handle: RecordingWriter, policy: SealPolicy) -> Self {
        Self {
            accounts: BTreeMap::new(),
            last_keys: BTreeMap::new(),
            described: BTreeMap::new(),
            schemas: HashMap::new(),
            segment_schema: BTreeMap::new(),
            warned: HashSet::new(),
            schema_stats: SchemaCacheStats::default(),
            handle,
            policy,
        }
    }

    /// The recording being written — a valid, readable `.rez` throughout.
    /// The archive this recording is being written into.
    ///
    /// Test-only today: the recorder asks its `RezArchive` directly, since one
    /// archive now backs several recorders and the path is a property of the
    /// archive rather than of any one recording.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> &Path {
        self.handle.path()
    }

    /// V3 schema-hash cache hit/miss counts so far — see [`SchemaCacheStats`].
    #[cfg(test)]
    pub fn schema_cache_stats(&self) -> SchemaCacheStats {
        self.schema_stats
    }

    /// Append one scraped snapshot: partition by sampler and, for each sampler
    /// whose representative acquisition window advanced, push a row stamped
    /// `anchored_ts` with this tick's `wall_offset_ns` observation — **and**
    /// hand the same row to the WAL.
    ///
    /// The WAL row and the builder row are produced from the same entries
    /// behind the same dedup gate. A tick the segment path skipped must not
    /// reappear as a recovered row, or replaying the WAL would put back a
    /// duplicate observation the dedup rule exists to remove.
    ///
    /// **Fallible, unlike v2's `ingest`**, because unlike v2's it writes. The
    /// alternative — stash the error and report it from the next `maybe_seal` —
    /// can swallow it outright: `RecordingWriter::send` reports the writer error on a send
    /// failure, so the subsequent `Drop` finds nothing to join, logs nothing,
    /// and a caller that never calls `maybe_seal` or `finalize` again loses the
    /// failure entirely. The caller already handles `maybe_seal`'s error; this
    /// is the same shape at the same cadence.
    ///
    /// Note that a writer failure is still *asynchronous* — the hand-off can
    /// return `Ok` for the tick that ultimately kills the writer, and the error
    /// surfaces on a later hand-off. That is inherent to the writer thread and
    /// is why `maybe_seal` polls health even with nothing to seal.
    pub fn ingest(
        &mut self,
        snapshot: &Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        let rows = self.stage(snapshot, anchored_ts, wall_offset_ns)?;
        self.handle.wal(rows)
    }

    /// Build this tick's WAL rows for THIS recording without committing them.
    ///
    /// The archive-level half of [`ingest`](Self::ingest): a caller holding
    /// several recordings stages each one and commits the tick once, through
    /// [`RezArchive::wal_tick`], so an archive pays one transaction — one fsync
    /// at `synchronous=FULL` — per tick rather than one per endpoint.
    ///
    /// Everything except the hand-off happens here, dedup and seal accounting
    /// included, so the two spellings cannot drift on what a tick MEANS. The
    /// accounting advances even if the caller then fails to commit: that is
    /// unchanged from `ingest`, whose send could always fail after the same
    /// state moved.
    /// The `recordings` row this recorder appends to — the key a batched tick
    /// commit is addressed by.
    pub fn recording_id(&self) -> i64 {
        self.handle.recording_id()
    }

    pub fn stage(
        &mut self,
        snapshot: &Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<Vec<WalRow>, String> {
        // Native V3 ingest is keyed by GROUP, not sampler, and its WAL payload
        // shape (`WalGroupRow`) differs entirely from V1/V2's `WalCell`s — so
        // it is its own path rather than a fork inside the loop below.
        // `group_by_sampler` returns nothing for a `Snapshot::V3` (it cannot
        // borrow `Entry`s out of a group's raw value slots), so this early
        // return is also what keeps the V1/V2 loop below untouched and
        // byte-identical: it simply never sees a V3 snapshot.
        if let Snapshot::V3(v3) = snapshot {
            return self.ingest_v3(&v3.groups, anchored_ts, wall_offset_ns);
        }
        // One `Vec` for the whole tick: `RecordingWriter::wal` commits it as a
        // single transaction, so a tick is atomic across samplers.
        //
        // `wal`'s primary key is `(recording_id, sampler, ts)`, so re-using an
        // `anchored_ts` for one sampler is a UNIQUE violation that kills the
        // recording — stricter than the segment path, which would merely write
        // two rows at one timestamp. Unreachable from the recorder loop, whose
        // stamps are a monotonic clock in nanoseconds, and left strict on
        // purpose: a repeated stamp means the caller's clock is broken, and
        // `ON CONFLICT DO NOTHING` would silently drop the tick instead.
        //
        // Two passes, and the split is deliberate: everything fallible happens
        // in the first, which touches no builder. `encode_wal_row` returning
        // `Err` mid-tick would otherwise leave the samplers already visited
        // holding a builder row whose WAL row was never committed — the one
        // state this design must not produce.
        let mut wal_rows = Vec::new();
        let mut accepted = Vec::new();
        for (sampler, entries) in group_by_sampler(snapshot) {
            // `is_group_table_key` (`materialize_wal_tail`'s dispatch between
            // this sampler-cell path and `ingest_v3`'s group-row path) relies
            // entirely on a V1/V2 sampler key never containing `/` — see its
            // doc. This assert guards drift in THIS build's own registered
            // sampler names (`no_registered_sampler_name_contains_a_slash`
            // pins that half); it is not a wire-input validator — `sampler_of`
            // reads the `"sampler"` metadata key straight off the wire,
            // unvalidated, so a hostile or merely unusual endpoint could still
            // reach this with `/` in a debug-off (release) build. The
            // structural non-aliasing of `WalGroupRow` vs `Vec<WalCell>` is
            // the backstop that actually covers that case: a decode error,
            // not silent misrouting, either way.
            debug_assert!(
                !sampler.contains('/'),
                "sampler key {sampler:?} contains '/', which materialize_wal_tail's \
                 is_group_table_key reserves for V3 acquisition-group table keys — \
                 this sampler would be misrouted to the group decode path"
            );
            let key = dedup_key(&entries, anchored_ts);
            if let Some(&last) = self.last_keys.get(sampler) {
                if key <= last {
                    continue; // window unchanged → same observation → skip
                }
            }

            let described = self.described.entry(sampler.to_string()).or_default();
            let cells: Vec<WalCell> = entries
                .iter()
                .map(|e| {
                    let name = e.name().to_string();
                    // `insert` returns true the first time only — and
                    // `maybe_seal` empties this set when the sampler seals, so
                    // "the first time" means "the first time in this segment".
                    let metadata = described.insert(name.clone()).then(|| {
                        e.metadata()
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    });
                    WalCell {
                        name,
                        metadata,
                        value: WalValue::of(e),
                        window: e.window().map(|w| (w.begin_ns, w.end_ns)),
                    }
                })
                .collect();
            wal_rows.push(WalRow {
                sampler: sampler.to_string(),
                ts: anchored_ts,
                wall_offset: wall_offset_ns,
                row: encode_wal_row(&cells)?,
            });
            accepted.push((sampler, key, entries));
        }

        // Pass 2: infallible. The accounts and the dedup keys advance together.
        // Only the accounting advances here — the values themselves are in
        // `wal_rows`, committed below.
        let stagger_key = self.handle.stagger_key().to_string();
        for (sampler, key, entries) in accepted {
            self.last_keys.insert(sampler.to_string(), key);
            let policy = &self.policy;
            let stagger_key = stagger_key.as_str();
            self.accounts
                .entry(sampler.to_string())
                .or_insert_with(|| SegmentAccount::open_first(sampler, stagger_key, policy))
                .add_row(entries_approx_bytes(&entries));
        }
        Ok(wal_rows)
    }

    /// The native V3 path: one WAL row per acquisition group whose window
    /// advanced, keyed by the group's own name (`"<sampler>/<group>"` —
    /// already that string in the payload). This is what makes the WAL/table
    /// key a GROUP rather than a sampler for V3: the `wal` table's key
    /// column is still named `sampler` (see `rez_sqlite`'s schema) and is
    /// reused as the generic table key rather than migrated — a V3 group's
    /// key simply happens to contain a `/`, which is also how
    /// `is_group_table_key` tells the two shapes apart later. Migrating the
    /// schema (e.g. a `kind` column) was considered and rejected: it buys
    /// nothing a string convention doesn't already provide (WAL rows are
    /// opaque BLOBs either way, and the two payload shapes are structurally
    /// distinguishable — `WalGroupRow` vs `Vec<WalCell>` fail to
    /// cross-decode instead of aliasing), and it would touch every SQL
    /// statement in `rez_sqlite.rs` for no behavioral gain.
    ///
    /// Same two-pass shape as `ingest`, same reason: everything fallible
    /// (encoding a WAL row) happens before anything infallible (advancing
    /// `last_keys`/`accounts`) is touched, so a mid-tick encode failure never
    /// leaves the dedup/account state ahead of what was actually committed.
    fn ingest_v3(
        &mut self,
        groups: &[GroupSnapshot],
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<Vec<WalRow>, String> {
        let mut wal_rows = Vec::new();
        let mut accepted: Vec<(&str, u64, usize)> = Vec::new();
        // Names already accepted THIS TICK. `group_by_sampler`'s V1/V2 path is
        // structurally immune to a duplicate key — it groups into a
        // `BTreeMap`, so a repeated sampler label just merges into one
        // entry's entries — but V3 pushes one `WalRow` per group as it
        // iterates the payload's own `Vec<GroupSnapshot>`, keyed by
        // `g.name`. Two groups sharing a name in ONE tick would both pass the
        // window-advance dedup below (it only compares against the PREVIOUS
        // tick's key in `last_keys`, which pass 2 has not updated yet) and
        // both get pushed into `wal_rows` with the same `(sampler, ts)` —
        // a `wal` primary-key violation that fails the whole tick and kills
        // the writer thread (`RezDb::insert_wal_rows` is one transaction).
        // The rezolus agent cannot produce this (acquisition-group names are
        // unique by construction), but `.rez` mode accepts ANY msgpack
        // endpoint, and nothing about the WAL's PK-uniqueness reasoning
        // (a monotonic recorder clock makes `ts` unique) says anything about
        // a producer-supplied, possibly-duplicated `name`. First occurrence
        // wins; every later one this tick is skipped with a rate-limited
        // warning instead of taking the whole tick down.
        let mut seen_this_tick: HashSet<&str> = HashSet::new();
        for g in groups {
            // The other half of the invariant `is_group_table_key` rests on:
            // a V3 group name is always `"<sampler>/<group>"` — see the
            // matching assertion (and its rationale) in `ingest`'s V1/V2
            // loop, above.
            debug_assert!(
                g.name.contains('/'),
                "group name {:?} contains no '/', which materialize_wal_tail's \
                 is_group_table_key requires to route a V3 acquisition-group table \
                 to the group decode path — this group would be misrouted to the \
                 V1/V2 sampler-cell path",
                g.name
            );
            if !seen_this_tick.insert(g.name.as_str()) {
                if self.warned.insert(format!("{}#duplicate-in-tick", g.name)) {
                    warn!(
                        "group {} appears more than once in one tick; keeping the first \
                         occurrence and skipping the rest (warned once)",
                        g.name
                    );
                }
                continue;
            }
            // Dedup FIRST, exactly as the V1/V2 path does (`ingest`, above):
            // a window that has not advanced is the same observation again,
            // so it costs nothing further — no validation, no schema/cache
            // work, no warning. This is the "window-advance dedup is now
            // exact per group" §18 promised: one key per group, not the
            // sampler-wide max this replaces.
            let key = g.window.map(|w| w.end_ns).unwrap_or(anchored_ts);
            if let Some(&last) = self.last_keys.get(g.name.as_str()) {
                if key <= last {
                    continue;
                }
            }

            // Validate BEFORE caching (banked adversarial requirement): a
            // group that fails `GroupSnapshot::validate()` is skipped for
            // this tick and its schema — if any — is NEVER inserted into
            // `self.schemas`. `last_keys`/`accounts` are untouched too (this
            // group is simply absent from `accepted`), so a failing group
            // does not consume its own dedup slot and can retry next tick.
            if let Err(e) = g.validate() {
                if self.warned.insert(format!("{}#invalid", g.name)) {
                    warn!(
                        "group {} failed validation ({e:?}); skipping until it recovers \
                         (warned once)",
                        g.name
                    );
                }
                continue;
            }

            // Resolve the schema: a transmitted schema either teaches the
            // cache something new (miss) or confirms what it already knew
            // (hit); a `schema: None` payload must resolve from the cache — a
            // miss there means the agent believes we already have a schema
            // we do not, which the doc on `schemas` calls a producer bug or a
            // truncated payload, not steady state (today's producer always
            // sends the schema, so this path is exercised only by malformed
            // input in practice).
            let schema: Arc<GroupSchema> = match &g.schema {
                Some(s) => {
                    // `get_mut` first — a `&str` lookup, no allocation — and
                    // only fall to `entry`'s owned key (one `String` clone)
                    // on the genuine first sighting of this group name,
                    // rather than on every hit-or-miss tick. A schema-bearing
                    // tick is common (every re-anchor sends one — see
                    // `StreamRecorderV3::segment_schema` — and today's
                    // producer always includes it besides), so this was a
                    // per-tick allocation this arc's own `described`/dedup
                    // cleanup was supposed to have retired.
                    if let Some(ring) = self.schemas.get_mut(g.name.as_str()) {
                        if ring.iter().any(|(hash, _)| *hash == g.schema_hash) {
                            self.schema_stats.hits += 1;
                        } else {
                            ring.push_back((g.schema_hash, Arc::clone(s)));
                            // Evict the oldest generation once the ring runs
                            // over its cap — see `SCHEMA_RING_LEN`'s doc for
                            // why 3 is enough (the agent itself only ever
                            // needs the newest).
                            if ring.len() > SCHEMA_RING_LEN {
                                ring.pop_front();
                            }
                            self.schema_stats.misses += 1;
                        }
                    } else {
                        let mut ring = SchemaRing::new();
                        ring.push_back((g.schema_hash, Arc::clone(s)));
                        self.schemas.insert(g.name.clone(), ring);
                        self.schema_stats.misses += 1;
                    }
                    Arc::clone(s)
                }
                None => match self
                    .schemas
                    .get(g.name.as_str())
                    .and_then(|ring| ring.iter().find(|(hash, _)| *hash == g.schema_hash))
                {
                    Some((_, s)) => {
                        self.schema_stats.hits += 1;
                        Arc::clone(s)
                    }
                    None => {
                        if self.warned.insert(format!("{}#unknown-schema", g.name)) {
                            warn!(
                                "group {} sent schema: None for an unresolved schema hash \
                                 {:?}; skipping (producer bug, a truncated payload, or a \
                                 generation older than this recorder's schema ring, warned \
                                 once)",
                                g.name, g.schema_hash
                            );
                        }
                        continue;
                    }
                },
            };

            // `validate()` only checks arity against a TRANSMITTED schema
            // (it returns `Ok` outright when `g.schema` is `None`); a schema
            // resolved from the cache still needs the same check, since a
            // buggy producer could send a `schema: None` payload whose value
            // vectors do not actually match the cached schema's arity.
            if schema.counters.len() != g.counters.len()
                || schema.gauges.len() != g.gauges.len()
                || schema.histograms.len() != g.histograms.len()
            {
                if self.warned.insert(format!("{}#arity", g.name)) {
                    warn!(
                        "group {} values do not match its resolved schema's arity; skipping \
                         (warned once)",
                        g.name
                    );
                }
                continue;
            }

            // Per-segment re-anchor: does the CURRENT segment's live WAL span
            // already carry this exact schema for this group? Independent of
            // whether the AGENT sent it this tick — a fresh segment (just
            // rotated, see `maybe_seal`'s `segment_schema.remove`) has never
            // seen it, so the WAL row anchors it again even on an agent-side
            // cache hit. This is what keeps a group's WAL rows self-sufficient
            // for `materialize_wal_tail`, called by a reader process with no
            // access to this cache at all.
            let need_anchor = self.segment_schema.get(g.name.as_str()) != Some(&g.schema_hash);
            if need_anchor {
                self.segment_schema.insert(g.name.clone(), g.schema_hash);
            }

            let row = WalGroupRow {
                schema_hash: g.schema_hash,
                // The ingest boundary: the producer's schema becomes the
                // archive's on the way into the WAL.
                schema: need_anchor.then(|| schema.as_ref().into()),
                window: g.window.map(|w| (w.begin_ns, w.end_ns)),
                counters: g.counters.clone(),
                gauges: g.gauges.clone(),
                histograms: g
                    .histograms
                    .iter()
                    .map(|h| {
                        h.as_ref().map(|h| {
                            (
                                h.config().grouping_power(),
                                h.config().max_value_power(),
                                h.as_slice().to_vec(),
                            )
                        })
                    })
                    .collect(),
            };
            wal_rows.push(WalRow {
                sampler: g.name.clone(),
                ts: anchored_ts,
                wall_offset: wall_offset_ns,
                row: encode_wal_group_row(&row)?,
            });
            accepted.push((g.name.as_str(), key, group_approx_bytes(g)));
        }

        // Pass 2: infallible, same shape as `ingest`'s.
        let stagger_key = self.handle.stagger_key().to_string();
        for (name, key, bytes) in accepted {
            self.last_keys.insert(name.to_string(), key);
            let policy = &self.policy;
            let stagger_key = stagger_key.as_str();
            self.accounts
                .entry(name.to_string())
                .or_insert_with(|| SegmentAccount::open_first(name, stagger_key, policy))
                .add_row(bytes);
        }
        Ok(wal_rows)
    }

    /// Seal every open segment past any threshold, as ONE batch → one
    /// transaction. Empty segments never seal.
    ///
    /// Call this every loop iteration, scrape or not: an unreachable endpoint
    /// must still get its pre-outage rows sealed, and it is also where a writer
    /// that died asynchronously gets noticed.
    ///
    /// **The rows are not here, so the batch names samplers rather than
    /// carrying tables.** The writer thread reads each sampler's live WAL and
    /// rebuilds its segment from that. Which rows those are is settled by
    /// ordering rather than by a snapshot taken here: the channel is FIFO and
    /// the writer single-threaded, so every WAL row handed off before this
    /// message is committed before it is read, and every row after it is not
    /// yet visible to `live_wal`'s watermark. The segment therefore covers
    /// exactly the rows this decision was made about.
    pub fn maybe_seal(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let mut batch = Vec::new();
        for (sampler, account) in self.accounts.iter_mut() {
            if !account.is_due(now) {
                continue;
            }
            account.rotate(&self.policy, now);
            // The metadata anchor is per SEGMENT, so it rotates with the
            // account: the next WAL row for this sampler re-carries every
            // metric's metadata. That is what keeps the live WAL
            // self-contained once the prune below the new segment's `last_ts`
            // lands, and what makes the WAL capture label drift exactly where
            // the rebuilt table captures it. `segment_schema` is the V3
            // group-table equivalent of the same rule — a no-op entry to
            // remove for a V1/V2 sampler key, which never appears in it.
            self.described.remove(sampler);
            self.segment_schema.remove(sampler);
            batch.push(sampler.clone());
        }
        self.handle.seal(batch)
    }

    /// Apply retention: everything wholly older than `cutoff_ts` goes.
    ///
    /// The recorder never calls this — a recording keeps everything it records
    /// — but hindsight is the same writer with retention configured, and this
    /// is the configuration. Call it AFTER `maybe_seal`, so a segment closed
    /// this tick is catalogued before the cutoff is applied to it.
    pub fn evict_before(&mut self, cutoff_ts: u64) -> Result<(), String> {
        self.handle.evict_before(cutoff_ts)
    }

    /// Block until the writer has committed everything handed off so far; see
    /// [`RecordingWriter::sync`]. Needed before reading the file through a second
    /// connection, which otherwise sees the state from before the last tick.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sync(&mut self) -> Result<(), String> {
        self.handle.sync()
    }

    /// Seal the remaining partial segments (small by construction) and mark the
    /// recording complete.
    ///
    /// The tails are sealed even though the WAL already holds them and the
    /// reader can materialize that tail: a cleanly finished recording should be
    /// segments and nothing else, so the WAL is left empty and no consumer pays
    /// for a replay it does not need.
    pub fn finalize(mut self, clock_offset: (u64, i64)) -> Result<(), String> {
        let tails: Vec<String> = std::mem::take(&mut self.accounts)
            .into_iter()
            .filter(|(_, account)| account.rows() > 0)
            .map(|(sampler, _)| sampler)
            .collect();
        self.handle.seal(tails)?;
        self.handle.finalize(clock_offset)
    }

    /// Rows in a sampler's open (unsealed) segment.
    #[cfg(test)]
    fn open_rows(&self, sampler: &str) -> usize {
        self.accounts
            .get(sampler)
            .map(SegmentAccount::rows)
            .unwrap_or(0)
    }

    /// The row and age targets the sampler's *current* open segment seals at.
    #[cfg(test)]
    fn open_targets(&self, sampler: &str) -> Option<(usize, std::time::Duration)> {
        self.accounts.get(sampler).map(SegmentAccount::targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rez::recorder_tests_support::counter;
    use crate::rez::write_table_parquet;
    use crate::rez::{detect_rez_format, Entry, RezFormat, TableBuilder};
    use crate::wal::{decode_wal_group_row, decode_wal_row};
    use crate::window::Window;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;

    fn seed() -> ManifestSeed {
        ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "100".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        }
    }

    /// Commit `ts.len()` WAL rows for `sampler`, every row carrying
    /// `wall_offset` in the `:wall_offset` sidecar.
    ///
    /// This is the whole of a segment's input now: sealing reads the live WAL
    /// back rather than being handed a table, so a test that wants a sealable
    /// segment writes the rows a tick would have written and then names the
    /// sampler. Metadata rides the first row only, as `ingest` anchors it.
    fn commit_wal(writer: &mut RecordingWriter, sampler: &str, ts: &[u64], wall_offset: i64) {
        let rows: Vec<WalRow> = ts
            .iter()
            .enumerate()
            .map(|(i, &t)| WalRow {
                sampler: sampler.to_string(),
                ts: t,
                wall_offset,
                row: encode_wal_row(&[WalCell {
                    name: "0".to_string(),
                    metadata: (i == 0).then(|| {
                        [("sampler".to_string(), sampler.to_string())]
                            .into_iter()
                            .collect()
                    }),
                    value: WalValue::Counter(i as u64),
                    window: Some((t - 1, t)),
                }])
                .unwrap(),
            })
            .collect();
        writer.wal(rows).unwrap();
    }

    /// `commit_wal` at the offset most tests do not care about.
    fn commit(writer: &mut RecordingWriter, sampler: &str, ts: &[u64]) {
        commit_wal(writer, sampler, ts, 7);
    }

    /// WAL rows the writer cannot turn into a segment: the bucket vector does
    /// not match the H2 config it is tagged with, so `Histogram::from_buckets`
    /// rejects it inside `materialize_wal_tail`. Same shape of mid-recording
    /// writer failure as a full disk — it happens on the writer thread, after
    /// the hand-off returned.
    fn commit_unencodable(writer: &mut RecordingWriter, sampler: &str) {
        let row = WalRow {
            sampler: sampler.to_string(),
            ts: 1_000,
            wall_offset: 0,
            row: encode_wal_row(&[WalCell {
                name: "0".to_string(),
                metadata: None,
                value: WalValue::Histogram(7, 64, vec![1, 2, 3]),
                window: None,
            }])
            .unwrap(),
        };
        writer.wal(vec![row]).unwrap();
    }

    /// One committed WAL row. The payload is a real encoded cell rather than a
    /// placeholder: sealing decodes the live WAL to build the segment, so a row
    /// that cannot be decoded is a row that cannot be sealed.
    fn wal_row(sampler: &str, ts: u64) -> WalRow {
        WalRow {
            sampler: sampler.to_string(),
            ts,
            wall_offset: 7,
            row: encode_wal_row(&[WalCell {
                name: "0".to_string(),
                metadata: None,
                value: WalValue::Counter(ts),
                window: Some((ts.saturating_sub(1), ts)),
            }])
            .unwrap(),
        }
    }

    /// THE property the batched tick exists for: an archive's commit count per
    /// tick does not grow with its recording count.
    ///
    /// At `synchronous=FULL` a commit is an fsync, and the hand-off is a
    /// blocking send from inside the scrape loop — so a commit per recording
    /// put a linear-in-endpoint-count fsync bill on the loop that has to keep
    /// up with the sampling interval. `seal_batch` already refused that trade
    /// within one recording ("12 implicit commits would be 12 fsyncs at
    /// `synchronous=FULL` against a ~46 ms tick"); this is the same argument
    /// across recordings.
    ///
    /// Asserted as ONE commit for FOUR recordings, and separately that a
    /// 4-recording tick costs the same as a 1-recording tick — a count alone
    /// could be satisfied by a writer that committed once and dropped three
    /// recordings' rows, so the row check below is not decoration.
    #[test]
    fn a_tick_costs_one_commit_however_many_recordings_it_spans() {
        fn commits_for(recordings: usize, dir: &Path) -> (u64, usize) {
            let path = dir.join(format!("{recordings}.rez"));
            let mut archive = RezArchive::create(&path).unwrap();
            let mut recs: Vec<StreamRecorderV3> = (0..recordings)
                .map(|i| {
                    let mut seed = seed();
                    seed.labels.insert("source".to_string(), format!("svc{i}"));
                    StreamRecorderV3::new(archive.add_recording(seed).unwrap())
                })
                .collect();

            // Baseline AFTER the recordings exist: `add_recording` commits, and
            // what is being measured is the per-TICK cost.
            let baseline = archive.commits_for_test();

            let ts = 1_000_000_000u64;
            let staged: Vec<(i64, Vec<WalRow>)> = recs
                .iter_mut()
                .map(|rec| {
                    let rows = rec
                        .stage(
                            &snap(ts, vec![counter("cpu_cycles", "cpu_usage", 1, None)]),
                            ts,
                            0,
                        )
                        .unwrap();
                    (rec.recording_id(), rows)
                })
                .collect();
            archive.wal_tick(staged).unwrap();

            // `commits_for_test` is itself the barrier: the writer answers it
            // only after the tick ahead of it is committed.
            let commits = archive.commits_for_test() - baseline;
            let rows: usize = {
                let db = RezDb::open(&path).unwrap();
                (0..recordings)
                    .map(|i| db.read_wal(i as i64 + 1, "cpu_usage").unwrap().len())
                    .sum()
            };
            drop(recs);
            drop(archive);
            (commits, rows)
        }

        let dir = tempfile::tempdir().unwrap();
        let (one_commit, one_rows) = commits_for(1, dir.path());
        let (four_commits, four_rows) = commits_for(4, dir.path());

        assert_eq!(one_commit, 1, "a one-recording tick is one commit");
        assert_eq!(
            four_commits, one_commit,
            "a four-recording tick must cost the same as a one-recording tick, \
             not four times as much"
        );
        // ...and all four recordings' rows are actually in it. Without this, a
        // writer that committed once and dropped three would pass above.
        assert_eq!(one_rows, 1);
        assert_eq!(four_rows, 4, "every recording's row must be in that commit");
    }

    /// THE guarantee this cadence exists for: a plain copy of a live archive
    /// stays close to current.
    ///
    /// SQLite commits into a `-wal` sidecar, so a copy of the archive alone —
    /// what a `cp`, or a browser upload, actually gets — sees only what has
    /// been checkpointed. The size-based autocheckpoint bounds the sidecar's
    /// BYTES and says nothing about its AGE: measured before this existed, a
    /// 2000-tick recording's plain copy was 123 ticks (~2 minutes at 1s)
    /// behind.
    ///
    /// Asserted as a difference against an un-checkpointed writer in the same
    /// test, not against a constant, so it stays meaningful whatever the
    /// fixture's row sizes are.
    #[test]
    fn a_plain_copy_of_a_live_archive_keeps_up_when_the_writer_checkpoints() {
        fn last_row_in_a_plain_copy(checkpoint_every: Duration, dir: &Path) -> Option<u64> {
            let live = dir.join(format!("live-{}.rez", checkpoint_every.as_millis()));
            let mut archive =
                RezArchive::create_checkpointing_every(&live, checkpoint_every).unwrap();
            let mut writer = archive.add_recording(seed()).unwrap();
            let id = writer.recording_id();

            for t in 0..400u64 {
                let ts = 1_000_000_000 * (t + 1);
                writer
                    .wal(vec![WalRow {
                        sampler: "cpu_usage".to_string(),
                        ts,
                        wall_offset: 0,
                        row: encode_wal_row(&[WalCell {
                            name: "0".to_string(),
                            metadata: None,
                            value: WalValue::Counter(t),
                            window: None,
                        }])
                        .unwrap(),
                    }])
                    .unwrap();
            }
            writer.sync().unwrap();
            // Let the writer's timer fire at least once. Only meaningful for
            // the short interval; the long one has nothing to wait for.
            std::thread::sleep(Duration::from_millis(60));
            writer.sync().unwrap();

            let copy = dir.join(format!("copy-{}.rez", checkpoint_every.as_millis()));
            std::fs::copy(&live, &copy).unwrap();
            // `None` covers both "the copy has no rows for this table" and
            // "the copy has no catalog at all" — the un-checkpointed case can
            // be either, and both mean the same thing here: the copy did not
            // keep up.
            let reach = RezDb::open(&copy)
                .ok()
                .and_then(|db| db.live_wal_span(id, "cpu_usage").ok())
                .and_then(|s| s.last_ts);

            drop(writer);
            drop(archive);
            reach
        }

        let dir = tempfile::tempdir().unwrap();
        let last_ts = 1_000_000_000 * 400;

        // Checkpointing often: the copy reaches the last committed row.
        let checkpointed = last_row_in_a_plain_copy(Duration::from_millis(10), dir.path());
        assert_eq!(
            checkpointed,
            Some(last_ts),
            "a checkpointing writer must leave the archive itself current"
        );

        // Effectively never (the size-based autocheckpoint still applies, but
        // this fixture is far too small to trip it): the copy falls short. This
        // half is the premise — without it, the assertion above would pass on a
        // container that never needed the cadence.
        let stale = last_row_in_a_plain_copy(Duration::from_secs(3_600), dir.path());
        assert!(
            stale != Some(last_ts),
            "premise: without a checkpoint the copy should NOT be current, but it \
             reached {stale:?}"
        );
    }

    #[test]
    fn create_leaves_a_valid_openable_file_immediately() {
        // THE property that retires `.partial`, rename-aside and the rename at
        // finalize: the output path holds a valid `.rez` before a single row
        // is written, so an early-killed recording needs no staging file to be
        // recoverable and no consumer has to know a second path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, writer) = RezArchive::single(&path, seed()).unwrap();

        assert!(path.exists(), "the output path itself, not a .partial");
        assert!(
            !dir.path().join("out.rez.partial").exists(),
            "v3 has no staging file"
        );
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V3Sqlite);

        // And it is not merely a well-formed empty database: the recording is
        // already there to be read, by a second connection, while the writer
        // still holds the file.
        let db = RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].id, writer.recording_id());
        assert_eq!(recordings[0].meta.clock_anchor_wall_ns, ANCHOR);
        assert_eq!(
            recordings[0].meta.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
        assert!(
            !recordings[0].complete,
            "an in-progress recording is never complete"
        );
    }

    #[test]
    fn a_seal_batch_is_one_transaction() {
        // Without batching, a fleet co-seal of 12 tables is 12 implicit
        // commits — 12 fsyncs at synchronous=FULL — against a ~46 ms tick.
        // "One transaction" is only observable by failing ONE insert in the
        // batch and checking that the segment inserted before it, which would
        // have committed fine on its own, is gone too.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        // Plant the row the batch's SECOND segment collides with on the
        // primary key `(recording_id, sampler, seq)`, from another connection.
        {
            let db = RezDb::open(&path).unwrap();
            db.insert_segment(
                rid,
                "blockio",
                0,
                &SegmentMeta {
                    rows: 1,
                    first_ts: 1,
                    last_ts: 1,
                },
                b"planted",
            )
            .unwrap();
        }

        commit(&mut writer, "cpu_usage", &[1_000, 2_000]);
        commit(&mut writer, "blockio", &[10]);
        writer
            .seal(vec!["cpu_usage".to_string(), "blockio".to_string()])
            .unwrap();
        // Through the archive: the handle only queues the finalize, and the
        // failing insert happens on the writer thread, so the error is what
        // the join returns.
        let err = _archive
            .finalize_single(writer, (2_000, 7))
            .expect_err("the colliding insert must fail the recording");
        assert!(
            err.contains("failed to insert segment blockio#0"),
            "the writer's own error, not a generic one: {err}"
        );

        let db = RezDb::open(&path).unwrap();
        assert!(
            db.read_segments(rid, "cpu_usage").unwrap().is_empty(),
            "cpu_usage was inserted BEFORE the failing statement; one \
             transaction means it is rolled back with it"
        );
        let blockio = db.read_segments(rid, "blockio").unwrap();
        assert_eq!(blockio.len(), 1, "only the planted row survives");
        assert_eq!(blockio[0].bytes, b"planted");
    }

    #[test]
    fn seal_prunes_only_the_sealed_samplers_wal_and_only_up_to_last_ts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        for ts in [10, 20, 30] {
            writer
                .wal(vec![wal_row("cpu_usage", ts), wal_row("blockio", ts)])
                .unwrap();
        }
        // cpu_usage seals what is live at this point — 10..30 — and blockio
        // seals nothing. The tick at 40 is committed AFTER the seal request, so
        // the channel's ordering is what keeps it out of the segment: it is not
        // yet visible to `live_wal` when the writer reaches the seal.
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();
        writer
            .wal(vec![wal_row("cpu_usage", 40), wal_row("blockio", 40)])
            .unwrap();
        _archive.finalize_single(writer, (40, 7)).unwrap();

        let db = RezDb::open(&path).unwrap();
        let cpu_usage = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            cpu_usage.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![40],
            "pruned up to the segment's last_ts, and no further"
        );
        let blockio = db.read_wal(rid, "blockio").unwrap();
        assert_eq!(
            blockio.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![10, 20, 30, 40],
            "an unsealed sampler's WAL is untouched by another's prune"
        );
    }

    #[test]
    fn kill_before_finalize_leaves_every_ticks_rows_recoverable() {
        // THE headline guarantee, and the reason for the whole container
        // change. A quiet sampler still inside its first seal period recovered
        // NOTHING under v2 — 16 of 26 fleet tables were in exactly this state
        // at `kill -9`, because kill-safety was per-segment. Here every tick is
        // committed as it arrives, so a recording that never seals and never
        // finalizes still holds every row.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        for i in 1..=5u64 {
            writer.wal(vec![wal_row("drivehealth", i * 10)]).unwrap();
        }
        // No finalize, no seal — the writer just goes away.
        drop(writer);
        // Joins the writer: the handle alone no longer stops it.
        drop(_archive);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.all_samplers(rid).unwrap(),
            vec!["drivehealth"],
            "a sampler that never sealed is still discoverable"
        );
        let live = db.live_wal(rid, "drivehealth").unwrap();
        assert_eq!(
            live.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![10, 20, 30, 40, 50],
            "every ingested tick is recoverable"
        );
        // The payload survived intact, not just the row: decoding it is what a
        // reader materializing this tail has to do.
        assert_eq!(
            decode_wal_row(&live[0].row).unwrap()[0].value,
            WalValue::Counter(10)
        );
        assert!(
            !db.read_recordings().unwrap()[0].complete,
            "a recording killed before finalize is not complete"
        );
    }

    #[test]
    fn kill_before_finalize_still_has_clock_observations() {
        // Clock drift must survive the kill path, which is the path v3 exists
        // for. If only `finalize` contributed an observation, a killed
        // recording would have NONE — and reading drift back out of a sealed
        // segment means decoding its `:wall_offset` column from the parquet
        // BLOB, since `segments` catalogs only rows/first_ts/last_ts. v2 could
        // render drift straight from the manifest with no decode; the
        // `clock_offsets` rows are what keep that true here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        // The newest row belongs to the sampler named FIRST, and the older
        // sampler carries a wildly different offset — so a derivation that took
        // the last sampler's, or the batch's oldest row, or mixed one table's
        // timestamp with another's offset, is visible here.
        commit_wal(&mut writer, "cpu_usage", &[3_000], 7);
        commit_wal(&mut writer, "scheduler", &[1_000, 2_000], 99);
        writer
            .seal(vec!["cpu_usage".to_string(), "scheduler".to_string()])
            .unwrap();
        // Killed: no finalize.
        drop(writer);
        // Joins the writer: the handle alone no longer stops it.
        drop(_archive);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(3_000, 7)],
            "the batch's newest sealed row, paired with its OWN table's offset"
        );
        assert!(!db.read_recordings().unwrap()[0].complete);
    }

    #[test]
    fn finalize_drops_a_tick_observation_a_sealed_row_already_covers() {
        // Two conflicting offsets at one timestamp would make the series
        // unreadable, so the row-derived value wins: it is a projection of the
        // `:wall_offset` column the segment itself carries. Same rule as v2.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        commit_wal(&mut writer, "cpu_usage", &[1_000], 7);
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();
        _archive.finalize_single(writer, (1_000, -11)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(1_000, 7)],
            "one observation per timestamp, and the sealed row's wins"
        );
    }

    /// A recording's stagger bucket must follow its LABELS, not the order its
    /// endpoint appeared on the command line.
    ///
    /// This is the property that ruled out keying the stagger on
    /// `recordings.id` — an autoincrement rowid, so the same two agents
    /// recorded with the flags swapped would have segmented differently and a
    /// capture would not reproduce. It can only be tested here, where the id
    /// exists: `stagger_bucket` never sees it, so an equivalent assertion down
    /// in `seal_policy` is a tautology.
    #[test]
    fn stagger_key_follows_the_labels_not_the_open_order() {
        let seed_for = |source: &str| ManifestSeed {
            labels: [
                ("host".to_string(), "alpha".to_string()),
                ("source".to_string(), source.to_string()),
            ]
            .into_iter()
            .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: 1_000,
        };
        // Open the same two label sets in both orders, so each is recording 0
        // once and recording 1 once.
        let open_both = |first: &str, second: &str| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let mut archive = RezArchive::create(&path).unwrap();
            let a = archive.add_recording(seed_for(first)).unwrap();
            let b = archive.add_recording(seed_for(second)).unwrap();
            let out = [
                (
                    first.to_string(),
                    a.recording_id(),
                    a.stagger_key().to_string(),
                ),
                (
                    second.to_string(),
                    b.recording_id(),
                    b.stagger_key().to_string(),
                ),
            ];
            a.finalize((1_000, 0)).unwrap();
            b.finalize((1_000, 0)).unwrap();
            archive.join().unwrap();
            out
        };

        let forward = open_both("redis", "valkey");
        let reversed = open_both("valkey", "redis");

        // The ids DID swap — otherwise this proves nothing about independence
        // from them.
        assert_eq!(forward[0].0, "redis");
        assert_eq!(reversed[1].0, "redis");
        assert_ne!(
            forward[0].1, reversed[1].1,
            "redis must hold a different recording id in the two runs"
        );
        // ...and the stagger key did not.
        let key_of = |runs: &[(String, i64, String); 2], source: &str| {
            runs.iter()
                .find(|(s, _, _)| s == source)
                .map(|(_, _, k)| k.clone())
                .unwrap()
        };
        assert_eq!(
            key_of(&forward, "redis"),
            key_of(&reversed, "redis"),
            "the stagger key must follow the labels, not the open order"
        );
        assert_ne!(
            key_of(&forward, "redis"),
            key_of(&forward, "valkey"),
            "and the two arms must still differ from each other"
        );
    }

    #[test]
    fn one_recordings_observation_does_not_suppress_anothers() {
        // `observed` is keyed by recording, and that keying is load-bearing.
        // Every recording finalizes with the SAME clock offset, because the
        // recorder passes one `last_clock` to all of them. So if `observed`
        // were a single archive-wide set, recording A sealing a row at T would
        // mark T seen, and recording B — whose endpoint went dark and has no
        // row at T — would have its finalize observation dropped as a
        // duplicate. B would lose the only clock observation it has, the one
        // anchoring its whole series.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut archive = RezArchive::create(&path).unwrap();
        let mut a = archive.add_recording(seed()).unwrap();
        let b = archive.add_recording(seed()).unwrap();
        let (rid_a, rid_b) = (a.recording_id(), b.recording_id());

        // A seals a row at T, so T is observed for A.
        commit_wal(&mut a, "cpu_usage", &[1_000], 7);
        a.seal(vec!["cpu_usage".to_string()]).unwrap();
        // B has no rows at all — its endpoint went dark.
        a.finalize((1_000, -11)).unwrap();
        b.finalize((1_000, -11)).unwrap();
        archive.join().unwrap();

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_clock_offsets(rid_a).unwrap(),
            vec![(1_000, 7)],
            "A keeps the sealed row's offset, not the finalize one"
        );
        assert_eq!(
            db.read_clock_offsets(rid_b).unwrap(),
            vec![(1_000, -11)],
            "B must still get its finalize observation — A's identical \
             timestamp must not suppress it"
        );
    }

    #[test]
    fn writer_error_surfaces_on_the_next_handoff() {
        // A writer-thread failure must surface as the writer's OWN error on a
        // hand-off — that is what decides whether a broken recording gets
        // noticed instead of producing per-tick log spam.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();

        // The hand-off itself succeeds: the failure happens on the writer.
        commit_unencodable(&mut writer, "cpu_usage");
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();

        // The next send that finds the receiver gone joins and reports. A
        // bounded retry, because the channel buffers one message and the
        // writer fails asynchronously.
        let mut surfaced = None;
        for _ in 0..500 {
            commit(&mut writer, "scheduler", &[1_000]);
            match writer.seal(vec!["scheduler".to_string()]) {
                Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    surfaced = Some(e);
                    break;
                }
            }
        }
        let err = surfaced.expect("the writer's error must surface on a hand-off");
        assert!(
            err.contains("failed to encode a cpu_usage segment"),
            "the writer's stored error, not a generic send failure: {err}"
        );

        // Exit-on-first-error: nothing the writer accepted after the failure
        // reached the file, and the recording is not complete.
        let db = RezDb::open(&path).unwrap();
        let recording = &db.read_recordings().unwrap()[0];
        assert!(db.samplers(recording.id).unwrap().is_empty());
        assert!(!recording.complete);
    }

    #[test]
    fn writer_error_surfaces_on_an_empty_handoff() {
        // Seals are size/age driven, so most ticks hand over nothing. Without
        // a health check on that path, a dead writer would go unnoticed for as
        // long as nothing happens to be due.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        commit_unencodable(&mut writer, "cpu_usage");
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();

        let mut surfaced = None;
        for _ in 0..500 {
            match writer.seal(Vec::new()) {
                Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    surfaced = Some(e);
                    break;
                }
            }
        }
        let err = surfaced.expect("a dead writer must surface on an empty batch too");
        assert!(
            err.contains("failed to encode a cpu_usage segment"),
            "the writer's stored error, not a generic one: {err}"
        );
    }

    #[test]
    fn finalize_marks_the_recording_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        writer.wal(vec![wal_row("cpu_usage", 1_000)]).unwrap();
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();
        _archive.finalize_single(writer, (2_000, -11)).unwrap();

        // Still the same file at the same path — nothing was renamed into
        // place, because nothing was ever staged.
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V3Sqlite);
        let db = RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert!(
            recordings[0].complete,
            "finalize marks the recording complete"
        );
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(1_000, 7), (2_000, -11)],
            "the sealed batch's observation, then the loop's last one — which \
             covers a span no sealed row does, so it joins the series"
        );
        assert_eq!(db.total_rows(rid, "cpu_usage").unwrap(), 1);
    }

    #[test]
    fn segments_get_consecutive_seq_numbers_per_sampler() {
        // `read_segments` splices in `seq` order, so a writer that restarted
        // numbering — or shared one counter across samplers — would produce a
        // table the reader cannot reassemble.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (_archive, mut writer) = RezArchive::single(&path, seed()).unwrap();
        let rid = writer.recording_id();

        // Each round commits the rows that round's segment should cover, then
        // seals — the WAL tail is what the segment is, so the rows have to land
        // between seals rather than all up front.
        commit(&mut writer, "cpu_usage", &[10, 20]);
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();
        commit(&mut writer, "cpu_usage", &[30]);
        commit(&mut writer, "blockio", &[30]);
        writer
            .seal(vec!["cpu_usage".to_string(), "blockio".to_string()])
            .unwrap();
        commit(&mut writer, "cpu_usage", &[40]);
        writer.seal(vec!["cpu_usage".to_string()]).unwrap();
        _archive.finalize_single(writer, (40, 7)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_segments(rid, "cpu_usage")
                .unwrap()
                .iter()
                .map(|s| (s.seq, s.meta.rows, s.meta.first_ts, s.meta.last_ts))
                .collect::<Vec<_>>(),
            vec![(0, 2, 10, 20), (1, 1, 30, 30), (2, 1, 40, 40)],
        );
        assert_eq!(
            db.read_segments(rid, "blockio")
                .unwrap()
                .iter()
                .map(|s| s.seq)
                .collect::<Vec<_>>(),
            vec![0],
            "each sampler numbers its own segments from 0"
        );
    }

    #[test]
    fn reclaim_is_skipped_until_the_free_list_is_a_tenth_of_the_file() {
        // The threshold is a cost decision, so it is asked directly: removing
        // the guard from `reclaim_if_fragmented` changes what the writer PAYS,
        // not what the file ends up looking like, and a test that watched
        // `page_count` would stay green with the guard deleted. (It did — this
        // test exists because that assertion was found vacuous.)
        assert!(!should_reclaim(0, 1_000), "nothing free, nothing to do");
        // A healthy rolling buffer reuses freed pages, leaving a free list of a
        // percent or so. It must never pay for a reclaim.
        assert!(!should_reclaim(11, 1_000), "steady state must not trip it");
        assert!(!should_reclaim(100, 1_000), "exactly a tenth is not MORE");
        assert!(should_reclaim(101, 1_000), "just past a tenth does");
        // The case the whole mechanism exists for: a working set that shrank,
        // leaving most of the file parked on the free list.
        assert!(should_reclaim(940, 1_000), "a shrunken working set");
        // A free list larger than the file is nonsense, but the multiply must
        // not wrap into "don't reclaim" if it ever happens.
        assert!(should_reclaim(u32::MAX, 1_000));
    }

    #[test]
    fn reclaim_hands_pages_back_in_bounded_passes() {
        // Eviction alone keeps the file bounded — freed pages get reused — but
        // the bound is the HIGH-WATER mark, so a working set that shrinks hard
        // leaves most of the file parked on the free list and never returned.
        // This is the trickle that fixes that, and it has two
        // halves that both have to hold: it must NOT run when the free list is
        // noise (steady state, where it would be pure cost), and it must be
        // BOUNDED when it does run, so a shrink cannot put a multi-second
        // reclaim on a tick.
        use crate::rez_sqlite::RecordingMeta;

        let dir = tempfile::tempdir().unwrap();
        let mut db = RezDb::create(&dir.path().join("t.rez")).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: ANCHOR,
            })
            .unwrap();

        let blob = vec![0x5au8; 64 * 1024];
        for seq in 0..40u64 {
            db.insert_segment(
                rid,
                "cpu_usage",
                seq,
                &SegmentMeta {
                    rows: 1,
                    first_ts: seq,
                    last_ts: seq,
                },
                &blob,
            )
            .unwrap();
        }
        let full = db.pragma_u32("page_count").unwrap();

        // Nothing has been freed yet, so a pass cannot shrink the file. (This
        // says nothing about whether the GUARD ran — see
        // `reclaim_is_skipped_until_the_free_list_is_a_tenth_of_the_file`,
        // which is where that claim lives, because it is invisible here.)
        reclaim_if_fragmented(&db).unwrap();
        assert_eq!(
            db.pragma_u32("page_count").unwrap(),
            full,
            "a file with nothing on its free list has nothing to give back"
        );

        // Now shrink the working set hard — the case the mechanism exists for.
        db.evict_before(rid, 38).unwrap();
        let freed = db.pragma_u32("freelist_count").unwrap();
        assert!(
            freed * RECLAIM_FREELIST_DIVISOR > full,
            "fixture: {freed} free of {full} pages must trip the guard"
        );
        assert_eq!(
            db.pragma_u32("page_count").unwrap(),
            full,
            "deleting alone returns nothing to the filesystem — that is the \
             high-water problem this test is about"
        );

        // One pass is bounded: it hands back at most RECLAIM_PAGES_PER_PASS,
        // not the whole free list.
        reclaim_if_fragmented(&db).unwrap();
        let after_one = db.pragma_u32("page_count").unwrap();
        assert!(
            after_one < full,
            "a pass over a fragmented file must return pages: {full} -> {after_one}"
        );
        assert!(
            full - after_one <= RECLAIM_PAGES_PER_PASS + 1,
            "a pass must stay bounded, not reclaim {} pages at once",
            full - after_one
        );

        // And it keeps going until the file really has shrunk.
        for _ in 0..50 {
            reclaim_if_fragmented(&db).unwrap();
        }
        let settled = db.pragma_u32("page_count").unwrap();
        assert!(
            settled < full / 4,
            "the file must drain back toward its live size: {full} -> {settled}"
        );
        assert_eq!(
            db.read_segments(rid, "cpu_usage").unwrap().len(),
            2,
            "and the surviving segments are untouched"
        );
    }

    // ---------------------------------------------------------------------
    // `StreamRecorderV3` — the ingest side.
    // ---------------------------------------------------------------------

    use crate::rez::recorder_tests_support::snap;
    use metriken_exposition::{Counter, Gauge, Histogram as ExpHistogram, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn policy(max_rows: usize) -> SealPolicy {
        SealPolicy {
            max_bytes: usize::MAX,
            max_rows,
            max_age: Duration::from_secs(3600),
        }
    }

    /// A policy no builder can ever meet: only `finalize` closes a segment.
    fn never_seals() -> SealPolicy {
        policy(usize::MAX)
    }

    /// A recorder plus the archive that owns its writer thread.
    ///
    /// The archive MUST be returned rather than dropped here: dropping it
    /// stops the writer, so a helper that kept it would hand back a recorder
    /// whose thread had already exited. Callers that read the file mid-test
    /// join it with `drop(archive)` (or `finalize_single`), which is what
    /// flushes everything queued.
    fn recorder(path: &Path, policy: SealPolicy) -> (RezArchive, StreamRecorderV3, i64) {
        let (archive, writer) = RezArchive::single(path, seed()).unwrap();
        let rid = writer.recording_id();
        (archive, StreamRecorderV3::with_policy(writer, policy), rid)
    }

    /// One row per sampler per tick, every sampler's window advancing each
    /// tick so nothing dedups and every table grows at the same rate.
    fn multi_snap(samplers: &[&str], i: u64) -> (Snapshot, u64) {
        let ts = 10_000 + i * 1_000;
        let end = 9_500 + i * 1_000;
        let counters = samplers
            .iter()
            .map(|s| counter(&format!("{s}_ops"), s, i, Some(Window::new(end - 500, end))))
            .collect();
        (snap(ts, counters), ts)
    }

    /// Drive `rec` one tick at a time, returning the row count at which each
    /// sampler sealed its **first** segment. Mirrors the v2 helper.
    fn first_seal_rows(rec: &mut StreamRecorderV3, samplers: &[&str], ticks: u64) -> Vec<usize> {
        let mut out = vec![0usize; samplers.len()];
        for i in 0..ticks {
            let (s, ts) = multi_snap(samplers, i);
            rec.ingest(&s, ts, 0).unwrap();
            let before: Vec<usize> = samplers.iter().map(|n| rec.open_rows(n)).collect();
            rec.maybe_seal().unwrap();
            for (k, name) in samplers.iter().enumerate() {
                if out[k] == 0 && before[k] > 0 && rec.open_rows(name) == 0 {
                    out[k] = before[k];
                }
            }
        }
        out
    }

    fn cells(row: &WalRow) -> Vec<WalCell> {
        decode_wal_row(&row.row).unwrap()
    }

    #[test]
    fn every_tick_writes_wal_rows_even_when_nothing_seals() {
        // The kill-loss guarantee rests on the WAL being written per TICK,
        // independent of the seal schedule — that is the whole reason for the
        // container swap. Under a policy no builder can meet, not one segment
        // exists, and every tick must still be on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (archive, mut rec, rid) = recorder(&path, never_seals());

        let mut want = Vec::new();
        for i in 0..5u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, i as i64).unwrap();
            rec.maybe_seal().unwrap();
            want.push(ts);
        }
        assert_eq!(rec.open_rows("cpu_usage"), 5, "nothing sealed");
        // Drop, not finalize: finalize would seal the tails and hide the point.
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        assert!(
            db.samplers(rid).unwrap().is_empty(),
            "no segment was ever sealed"
        );
        let wal = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            wal.iter().map(|r| r.ts).collect::<Vec<_>>(),
            want,
            "every tick's row is in the WAL, in order"
        );
        assert_eq!(
            wal.iter().map(|r| r.wall_offset).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "each row carries its own tick's wall observation"
        );
        // The rows are the tick's values, not placeholders.
        for (i, row) in wal.iter().enumerate() {
            let c = cells(row);
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].name, "cpu_usage_ops");
            assert_eq!(c[0].value, WalValue::Counter(i as u64));
        }
    }

    #[test]
    fn sealing_prunes_only_the_sealed_samplers_wal() {
        // This is why WAL rows are per-sampler rather than whole snapshots:
        // one slow table (drivehealth seals every 300 s at fleet cadence) must
        // not pin every other sampler's tail, and a busy table's prune must
        // not take the slow one's rows with it. `drivehealth`'s single row sits
        // BELOW `cpu_usage`'s sealed watermark, so a prune that forgot which
        // sampler it was pruning would delete it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        // Bucket 29 of 64 against `max_rows = 4` reduces the target by
        // `(4 / 128) * 29 = 0`, so cpu_usage seals at exactly 4 rows.
        let (archive, mut rec, rid) = recorder(&path, policy(4));

        let quiet = Window::new(0, 500); // never advances → deduped after tick 0
        for i in 0..7u64 {
            let ts = 10_000 + i * 1_000;
            let end = 9_500 + i * 1_000;
            let s = snap(
                ts,
                vec![
                    counter(
                        "cpu_usage_ops",
                        "cpu_usage",
                        i,
                        Some(Window::new(end - 500, end)),
                    ),
                    counter("drivehealth_ops", "drivehealth", i, Some(quiet)),
                ],
            );
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        let segments = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(segments.len(), 1, "cpu_usage sealed exactly one segment");
        assert_eq!(segments[0].meta.last_ts, 13_000);
        assert!(
            db.read_segments(rid, "drivehealth").unwrap().is_empty(),
            "drivehealth never sealed"
        );

        assert_eq!(
            db.read_wal(rid, "cpu_usage")
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect::<Vec<_>>(),
            vec![14_000, 15_000, 16_000],
            "the sealed sampler is pruned up to its own segment's last_ts"
        );
        assert_eq!(
            db.read_wal(rid, "drivehealth")
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect::<Vec<_>>(),
            vec![10_000],
            "the unsealed sampler's row survives another sampler's prune, \
             even though its ts is below that sampler's watermark"
        );
    }

    #[test]
    fn dedup_and_stagger_carry_over_from_v2() {
        // Both landed in 8dd4f442 on main and must not regress here: `last_keys`
        // lives outside the builder so it survives a rotation, and the first
        // seal is staggered per sampler so equal-rate tables never co-seal.
        let dir = tempfile::tempdir().unwrap();

        // (a) dedup survives a builder rotation — and suppresses the WAL row
        //     too, or recovery would resurrect a row the segment path dropped.
        let path = dir.path().join("dedup.rez");
        let (archive, mut rec, rid) = recorder(&path, policy(2));
        for i in 0..4u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        assert_eq!(rec.open_rows("cpu_usage"), 0, "two full segments sealed");

        // Tick 3's window lives in an already-sealed segment. Re-observing it
        // must still dedup, which only holds if `last_key` survived rotation.
        let (dup, dup_ts) = multi_snap(&["cpu_usage"], 3);
        rec.ingest(&dup, dup_ts + 1, 0).unwrap();
        assert_eq!(rec.open_rows("cpu_usage"), 0, "dedup survived the seal");
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(db.total_rows(rid, "cpu_usage").unwrap(), 4);
        // 4 WAL rows written, all four pruned by the two seals, and the
        // deduped re-observation never became a fifth.
        assert!(
            db.read_wal(rid, "cpu_usage").unwrap().is_empty(),
            "a deduped tick writes no WAL row"
        );

        // (b) the first seal is staggered across samplers.
        const MAX_ROWS: usize = 256;
        let path = dir.path().join("stagger.rez");
        let (_archive, mut rec, _) = recorder(&path, policy(MAX_ROWS));
        let samplers = ["cpu_usage", "scheduler"];
        let sealed = first_seal_rows(&mut rec, &samplers, MAX_ROWS as u64);
        assert_ne!(
            sealed[0], sealed[1],
            "same ingest rate must still give different first-seal row counts"
        );
        for (k, name) in samplers.iter().enumerate() {
            assert!(
                sealed[k] >= MAX_ROWS / 2 && sealed[k] <= MAX_ROWS,
                "{name} first-sealed at {} rows, outside [{}, {MAX_ROWS}]",
                sealed[k],
                MAX_ROWS / 2
            );
        }
        assert_eq!(
            rec.open_targets("cpu_usage"),
            Some((MAX_ROWS, Duration::from_secs(3600))),
            "every segment after the first uses the full policy"
        );
    }

    #[test]
    fn a_quiet_sampler_that_never_seals_is_fully_recoverable() {
        // THE headline guarantee, end to end through the ingest path. At
        // `kill -9` 120 s into a v2 fleet recording, 16 of 26 tables recovered
        // NOTHING because kill-safety was per-segment and they had not sealed
        // one yet. Here a table that never seals and never finalizes still
        // holds every row it ever ingested.
        //
        // "Fully" is the load-bearing word, so this checks every property a
        // recovered table needs rather than just the row count: all three
        // metric shapes, each tick's own `wall_offset` (the clock observation
        // the `:wall_offset` column is built from), the acquisition windows the
        // uncertainty bands are computed from, and the labels — without which
        // the rows would recover as values with no identity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (archive, mut rec, rid) = recorder(&path, never_seals());

        const TICKS: u64 = 200;
        for i in 0..TICKS {
            let ts = 10_000 + i * 1_000;
            // A drifting, signed, non-zero offset: a fixed 0 would pass against
            // a writer that dropped the observation entirely.
            rec.ingest(
                &shapes_snap("drivehealth", ts, "nanoseconds", i),
                ts,
                i as i64 - 100,
            )
            .unwrap();
            rec.maybe_seal().unwrap();
        }
        // Killed: no finalize, no seal, nothing flushed on the way out.
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.all_samplers(rid).unwrap(),
            vec!["drivehealth"],
            "a sampler with no segment at all is still discoverable"
        );
        assert!(
            db.read_segments(rid, "drivehealth").unwrap().is_empty(),
            "nothing sealed, so the WAL is the ONLY record"
        );
        let live = db.live_wal(rid, "drivehealth").unwrap();
        assert_eq!(live.len() as u64, TICKS, "every ingested tick is live");
        for (i, row) in live.iter().enumerate() {
            let i = i as u64;
            let ts = 10_000 + i * 1_000;
            assert_eq!(row.ts, ts);
            assert_eq!(
                row.wall_offset,
                i as i64 - 100,
                "each tick's own clock observation, not a shared one"
            );
            let c = cells(row);
            assert_eq!(
                c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                vec!["0", "1", "2"],
                "all three shapes recovered"
            );
            assert_eq!(c[0].value, WalValue::Counter(i));
            assert_eq!(c[1].value, WalValue::Gauge(-(i as i64)));
            let WalValue::Histogram(gp, mvp, ref buckets) = c[2].value else {
                panic!("the third metric is a histogram: {:?}", c[2].value);
            };
            assert_eq!((gp, mvp), (3, 8), "the H2 config rebuilds the histogram");
            assert_eq!(buckets.iter().sum::<u64>(), 1);
            assert_eq!(c[0].window, Some((ts - 500, ts)), "windows recovered");
        }
        // The labels are in the live WAL itself, so the recovered columns carry
        // the same identity a sealed segment's would.
        for c in cells(&live[0]) {
            let m = c.metadata.as_ref().expect("the first live row describes");
            assert_eq!(m.get("sampler").map(String::as_str), Some("drivehealth"));
            assert_eq!(m.get("unit").map(String::as_str), Some("nanoseconds"));
        }
        assert!(!db.read_recordings().unwrap()[0].complete);
    }

    fn shape_meta(metric: &str, sampler: &str, unit: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), sampler.to_string()),
            ("unit".to_string(), unit.to_string()),
        ]
        .into_iter()
        .collect()
    }

    /// One tick of `sampler` carrying all three metric shapes, with `unit` in
    /// every metric's metadata so a label change is observable.
    fn shapes_snap(sampler: &str, ts: u64, unit: &str, i: u64) -> Snapshot {
        let w = Some(Window::new(ts - 500, ts));
        let mut h = histogram::Histogram::new(3, 8).unwrap();
        h.increment(37).unwrap();
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters: vec![
                Counter::new("0".to_string(), i, shape_meta("0", sampler, unit))
                    .with_window(w.map(Into::into)),
            ],
            gauges: vec![
                Gauge::new("1".to_string(), -(i as i64), shape_meta("1", sampler, unit))
                    .with_window(w.map(Into::into)),
            ],
            histograms: vec![{
                // The agent's exposition puts the H2 config in a histogram's
                // metadata (`src/agent/exposition/http/snapshot.rs`), and
                // `read_table_parquet` needs it to rebuild the buckets — so a
                // fixture without it is not a snapshot this writer ever sees.
                let mut m = shape_meta("2", sampler, unit);
                m.insert("grouping_power".to_string(), "3".to_string());
                m.insert("max_value_power".to_string(), "8".to_string());
                ExpHistogram::new("2".to_string(), h, m).with_window(w.map(Into::into))
            }],
        })
    }

    /// One tick of `cpu_usage` carrying all three metric shapes.
    fn mixed_snap(ts: u64) -> Snapshot {
        shapes_snap("cpu_usage", ts, "nanoseconds", 42)
    }

    /// The `unit` label on a sealed segment's `"0"` column, decoded from the
    /// stored parquet BLOB.
    fn segment_unit(bytes: &[u8]) -> String {
        let table =
            crate::rez::read_table_parquet("cpu_usage".to_string(), bytes.to_vec()).unwrap();
        table
            .columns
            .iter()
            .find(|c| c.name == "0")
            .unwrap()
            .metadata["unit"]
            .clone()
    }

    #[test]
    fn each_segments_wal_span_re_anchors_its_own_metadata() {
        // The metadata anchor is per SEGMENT, not per recording, and that is
        // load-bearing three ways.
        //
        // 1. Label drift. `seal_completed` installs a fresh `TableBuilder`, so
        //    a segment column re-latches its labels at every rotation. An
        //    anchor held for the recording's lifetime would leave the WAL
        //    describing a recovered tail with the FIRST segment's stale labels
        //    while a sealed tail carried the new ones — the two paths
        //    disagreeing about the same rows.
        // 2. Self-containment. The first LIVE WAL row must carry the metadata,
        //    so recovery never decodes an old segment's footer to learn the
        //    tail's labels — the exact cost the WAL exists to avoid.
        // 3. Retention. Hindsight deletes segments by `last_ts < cutoff`
        //    (`rez_sqlite.rs`'s schema note); an anchor living in a deleted
        //    segment would put the metadata nowhere at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        // Bucket 29 against `max_rows = 2` reduces by `(2 / 128) * 29 = 0`.
        let (archive, mut rec, rid) = recorder(&path, policy(2));

        // Ticks 0-1 seal segment 0; the unit corrects at tick 2; ticks 2-3 seal
        // segment 1; tick 4 is the live tail.
        for i in 0..5u64 {
            let ts = 10_000 + i * 1_000;
            let unit = if i < 2 { "nanoseconds" } else { "microseconds" };
            rec.ingest(&shapes_snap("cpu_usage", ts, unit, i), ts, 0)
                .unwrap();
            rec.maybe_seal().unwrap();
        }
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        let segments = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(segments.len(), 2, "two rotations at max_rows = 2");
        assert_eq!(
            segments
                .iter()
                .map(|s| segment_unit(&s.bytes))
                .collect::<Vec<_>>(),
            vec!["nanoseconds", "microseconds"],
            "each segment re-latches its own labels — and they survive the \
             prune, being in the segment BLOB rather than the WAL"
        );

        let live = db.live_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            live.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![14_000],
            "only the tail is past the second segment's watermark"
        );
        for c in cells(&live[0]) {
            let m = c
                .metadata
                .as_ref()
                .unwrap_or_else(|| panic!("{} must re-carry metadata after a seal", c.name));
            assert_eq!(
                m.get("unit").map(String::as_str),
                Some("microseconds"),
                "the tail's labels are the CURRENT ones, and they are in the \
                 live WAL itself — no segment lookup, nothing lost to retention"
            );
        }
    }

    // THE claim the WAL-sourced seal rests on: a segment built by replaying the
    // WAL is the segment the buffered builder would have produced. Everything
    // downstream — the reader, `rate()` over a seal boundary, a v2/v3
    // comparison — assumes the two are interchangeable.
    //
    // Compared as encoded parquet bytes rather than field by field, because the
    // ways this can go wrong are all in the encoding: `WalCell::metadata` does
    // not carry `metric_type` (`push_row` injects it), and column ORDER is
    // insertion order, so a replay that assembled columns directly, or visited
    // cells in a different order, yields a segment a reader treats differently
    // while every individual value still matches.
    #[test]
    fn a_wal_sourced_segment_is_byte_identical_to_a_buffered_one() {
        let sampler = "cpu_usage";
        let ts = [1_000u64, 2_000, 3_000];

        // What the builder path produced: push each row, encode the table.
        let mut b = TableBuilder::new(sampler.to_string());
        for (i, &t) in ts.iter().enumerate() {
            let c = counter("0", sampler, i as u64, Some(Window::new(t - 1, t)));
            b.push_entries(t, 7, &[Entry::Counter(&c)]);
        }
        let buffered = write_table_parquet(&b.finish()).unwrap();

        // What the WAL path produces from the same ticks, metadata anchored on
        // the first row exactly as `ingest` anchors it.
        let rows: Vec<WalRow> = ts
            .iter()
            .enumerate()
            .map(|(i, &t)| WalRow {
                sampler: sampler.to_string(),
                ts: t,
                wall_offset: 7,
                row: encode_wal_row(&[WalCell {
                    name: "0".to_string(),
                    // The same metadata the entry carries above — `ingest`
                    // copies the snapshot entry's map into the cell verbatim.
                    metadata: (i == 0).then(|| {
                        [
                            ("metric".to_string(), "0".to_string()),
                            ("sampler".to_string(), sampler.to_string()),
                        ]
                        .into_iter()
                        .collect()
                    }),
                    value: WalValue::Counter(i as u64),
                    window: Some((t - 1, t)),
                }])
                .unwrap(),
            })
            .collect();
        let replayed = materialize_wal_tail(sampler, &rows).unwrap().unwrap();
        assert_eq!(replayed.rows, 3);
        assert_eq!(replayed.first_ts, 1_000);

        assert_eq!(
            replayed.bytes, buffered,
            "a segment replayed from the WAL must be what buffering would have \
             written, byte for byte"
        );
    }

    #[test]
    fn wal_rows_carry_each_metrics_metadata_once_then_values_only() {
        // The WAL row is the recovery record for a table that may never seal,
        // so it has to be self-describing — but repeating every metric's label
        // map on every tick is exactly the per-tick cost values-only rows exist
        // to avoid. Metadata therefore rides the FIRST
        // row a metric appears in and never again; by the time that row can be
        // pruned, a segment covering it carries the same metadata.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (archive, mut rec, rid) = recorder(&path, never_seals());

        rec.ingest(&mixed_snap(1_000), 1_000, 3).unwrap();
        rec.ingest(&mixed_snap(2_000), 2_000, 4).unwrap();
        drop(rec);
        // Joins the writer: dropping the handle alone no longer stops it.
        drop(archive);

        let db = RezDb::open(&path).unwrap();
        let wal = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(wal.len(), 2);

        let first = cells(&wal[0]);
        assert_eq!(
            first.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["0", "1", "2"],
            "counters, then gauges, then histograms — `group_by_sampler` order"
        );
        assert_eq!(first[0].value, WalValue::Counter(42));
        assert_eq!(first[1].value, WalValue::Gauge(-42));
        let WalValue::Histogram(gp, mvp, ref buckets) = first[2].value else {
            panic!("the third metric is a histogram: {:?}", first[2].value);
        };
        assert_eq!((gp, mvp), (3, 8), "the H2 config travels with the buckets");
        assert_eq!(buckets.iter().sum::<u64>(), 1);
        assert_eq!(first[0].window, Some((500, 1_000)));

        // Every metric's labels are on its first row, and nothing else needs to
        // be consulted to rebuild the column.
        for c in &first {
            let m = c
                .metadata
                .as_ref()
                .expect("first sighting carries metadata");
            assert_eq!(m.get("sampler").map(String::as_str), Some("cpu_usage"));
            assert_eq!(m.get("unit").map(String::as_str), Some("nanoseconds"));
        }

        let second = cells(&wal[1]);
        assert_eq!(second.len(), 3);
        for c in &second {
            assert!(
                c.metadata.is_none(),
                "{} repeated its metadata on the second tick",
                c.name
            );
        }
        assert_eq!(second[0].value, WalValue::Counter(42));
        assert_eq!(second[0].window, Some((1_500, 2_000)));
    }

    #[test]
    fn finalize_seals_the_tails_and_marks_the_recording_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (archive, mut rec, rid) = recorder(&path, never_seals());
        assert_eq!(rec.path(), path);

        for i in 0..3u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        archive.finalize_single_rec(rec, (12_000, 5)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert!(db.read_recordings().unwrap()[0].complete);
        assert_eq!(
            db.total_rows(rid, "cpu_usage").unwrap(),
            3,
            "the open tail is sealed rather than left for WAL replay"
        );
        assert!(
            db.read_wal(rid, "cpu_usage").unwrap().is_empty(),
            "sealing the tail prunes its WAL"
        );
    }

    // ---------------------------------------------------------------------
    // `StreamRecorderV3` — native V3 acquisition-group ingest.
    // ---------------------------------------------------------------------

    mod v3_groups {
        use super::*;
        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, SnapshotV3};

        fn desc(name: &str) -> MetricDesc {
            MetricDesc {
                name: name.to_string(),
                metadata: [("metric".to_string(), name.to_string())]
                    .into_iter()
                    .collect(),
            }
        }

        fn group_schema(members: &[&str]) -> GroupSchema {
            GroupSchema {
                counters: members.iter().map(|n| desc(n)).collect(),
                gauges: Vec::new(),
                histograms: Vec::new(),
            }
        }

        fn gauge_group_schema(members: &[&str]) -> GroupSchema {
            GroupSchema {
                counters: Vec::new(),
                gauges: members.iter().map(|n| desc(n)).collect(),
                histograms: Vec::new(),
            }
        }

        fn gauge_group_snapshot(
            name: &str,
            schema: &GroupSchema,
            gauges: Vec<Option<i64>>,
            window: Option<Window>,
        ) -> GroupSnapshot {
            GroupSnapshot {
                name: name.to_string(),
                schema_hash: schema.hash(),
                schema: Some(Arc::new(schema.clone())),
                window: window.map(Into::into),
                counters: Vec::new(),
                gauges,
                histograms: Vec::new(),
            }
        }

        fn group_snapshot(
            name: &str,
            schema: &GroupSchema,
            counters: Vec<Option<u64>>,
            window: Option<Window>,
            include_schema: bool,
        ) -> GroupSnapshot {
            GroupSnapshot {
                name: name.to_string(),
                schema_hash: schema.hash(),
                schema: include_schema.then(|| Arc::new(schema.clone())),
                window: window.map(Into::into),
                counters,
                gauges: Vec::new(),
                histograms: Vec::new(),
            }
        }

        fn v3_snap(ts: u64, groups: Vec<GroupSnapshot>) -> Snapshot {
            Snapshot::V3(SnapshotV3 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
                duration: Duration::ZERO,
                metadata: HashMap::new(),
                groups,
            })
        }

        /// A policy that seals a group's very first ingested row — `policy(1)`
        /// (defined above in the parent module): with `max_rows = 1` the
        /// first-seal stagger (`open_first`) reduces to exactly 1 regardless
        /// of the FNV bucket, since `1 - (1 / 128) * bucket == 1` for every
        /// bucket (integer division).
        fn seals_immediately() -> SealPolicy {
            policy(1)
        }

        /// A declared group member that is never written must reach the
        /// segment as a present, all-null nullable column — never as a
        /// fabricated `0`, and never silently dropped.
        ///
        /// The NVIDIA sampler manufactures exactly this on a Tegra SoC: the
        /// PCIe-derived gauges are deliberately not `set()`, while their
        /// siblings in the same acquisition group are. Before this, every V3
        /// writer test was counter-only (`group_schema` hardcodes
        /// `gauges: Vec::new()`), so the `Vec<Option<i64>>` -> `Int64Array`
        /// path was untested despite being on that sampler's live path.
        #[test]
        fn v3_group_gauge_member_that_is_never_written_stays_null() {
            use arrow::array::Array as _;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, rid) = recorder(&path, seals_immediately());

            let sch = gauge_group_schema(&["written", "never_written"]);
            let ts = 1_000_000_000u64;
            let window = Some(Window::new(ts - 50_000_000, ts));
            let g = gauge_group_snapshot(
                "gpu_nvidia/gpu_nvidia_devices",
                &sch,
                vec![Some(97), None],
                window,
            );
            rec.ingest(&v3_snap(ts, vec![g]), ts, 7).unwrap();
            rec.maybe_seal().unwrap();
            rec.sync().unwrap();

            let db = RezDb::open(&path).unwrap();
            let segs = db
                .read_segments(rid, "gpu_nvidia/gpu_nvidia_devices")
                .unwrap();
            assert_eq!(segs.len(), 1, "the single tick must have sealed");

            let mut reader =
                parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                    bytes::Bytes::from(segs[0].bytes.clone()),
                )
                .unwrap()
                .build()
                .unwrap();
            let batch = reader.next().unwrap().unwrap();

            // Both members are present as columns; membership comes from
            // registration, so gating a `set()` must not drop the column.
            let names: Vec<String> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            assert!(
                names.iter().any(|n| n == "never_written"),
                "unwritten member must still get a column, got {names:?}"
            );

            let idx = batch.schema().index_of("never_written").unwrap();
            let field = batch.schema().field(idx).clone();
            assert_eq!(field.data_type(), &arrow::datatypes::DataType::Int64);
            assert!(field.is_nullable(), "the column must be nullable");

            let col = batch
                .column(idx)
                .as_any()
                .downcast_ref::<arrow::array::Int64Array>()
                .expect("Int64 column");
            assert_eq!(col.len(), 1);
            assert!(
                col.is_null(0),
                "an unwritten gauge member must be null, not a fabricated 0"
            );

            // ...while its written sibling in the same group is unaffected.
            let widx = batch.schema().index_of("written").unwrap();
            let wcol = batch
                .column(widx)
                .as_any()
                .downcast_ref::<arrow::array::Int64Array>()
                .expect("Int64 column");
            assert!(!wcol.is_null(0));
            assert_eq!(wcol.value(0), 97);
        }

        #[test]
        fn v3_group_tick_round_trips_to_a_segment_with_exactly_the_expected_columns() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, rid) = recorder(&path, seals_immediately());

            let sch = group_schema(&["0", "1"]);
            let ts = 1_000_000_000u64;
            let window = Some(Window::new(ts - 50_000_000, ts));
            let g = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(10), Some(20)],
                window,
                true,
            );
            rec.ingest(&v3_snap(ts, vec![g]), ts, 7).unwrap();
            rec.maybe_seal().unwrap();
            rec.sync().unwrap();

            let db = RezDb::open(&path).unwrap();
            let segs = db.read_segments(rid, "cpu_usage/percpu").unwrap();
            assert_eq!(segs.len(), 1, "the single tick must have sealed");

            // The EXACT raw parquet column list: timestamp, :wall_offset, the
            // table-level window pair, then the group's two members — no
            // per-metric `<m>:window_begin`/`<m>:window_width` sidecars.
            let mut reader =
                parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                    bytes::Bytes::from(segs[0].bytes.clone()),
                )
                .unwrap()
                .build()
                .unwrap();
            let batch = reader.next().unwrap().unwrap();
            let schema_names: Vec<String> = batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            assert_eq!(
                schema_names,
                vec![
                    "timestamp",
                    ":wall_offset",
                    ":window_begin",
                    ":window_width",
                    "0",
                    "1",
                ]
            );

            // And the emitted window values decode back to the group's window.
            let table = crate::rez::read_table_parquet(
                "cpu_usage/percpu".to_string(),
                segs[0].bytes.clone(),
            )
            .unwrap();
            assert_eq!(
                table.table_window,
                Some(vec![window]),
                "the table-level window must decode back to what was ingested"
            );
            assert!(
                table
                    .columns
                    .iter()
                    .all(|c| c.windows.iter().all(Option::is_none)),
                "a group table's columns must not carry per-metric windows \
                 (no `<m>:window_begin`/`<m>:window_width` columns exist to \
                 decode, so every per-column window slot stays `None`)"
            );
        }

        #[test]
        fn window_advance_dedup_skips_an_unchanged_group() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            let sch = group_schema(&["0"]);
            let w = Some(Window::new(900, 1_000));
            for i in 0..3u64 {
                let g = group_snapshot("drivehealth/sweep", &sch, vec![Some(5)], w, i == 0);
                rec.ingest(&v3_snap(1_000 + i, vec![g]), 1_000 + i, 0)
                    .unwrap();
            }
            assert_eq!(
                rec.open_rows("drivehealth/sweep"),
                1,
                "the window never advanced past the first tick, so the other \
                 two must have been deduped"
            );
        }

        #[test]
        fn a_validate_failing_group_is_skipped_and_never_cached() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            let sch = group_schema(&["0", "1"]);
            // Arity mismatch: the schema declares two counters, the payload
            // carries one value — `GroupSnapshot::validate()` must reject it.
            let bad = group_snapshot("cpu_usage/percpu", &sch, vec![Some(1)], None, true);
            rec.ingest(&v3_snap(1_000, vec![bad]), 1_000, 0).unwrap();

            assert_eq!(
                rec.open_rows("cpu_usage/percpu"),
                0,
                "an invalid group must not produce a WAL row"
            );
            assert_eq!(
                rec.schema_cache_stats(),
                SchemaCacheStats::default(),
                "a validate()-failing group's schema must never enter the cache, \
                 hit or miss"
            );
        }

        #[test]
        fn a_schema_hash_hit_avoids_re_parse_and_a_miss_teaches_the_cache() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            let sch = group_schema(&["0"]);

            // Tick 1: schema included, never seen before -> miss.
            let g1 = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(1)],
                Some(Window::new(0, 100)),
                true,
            );
            rec.ingest(&v3_snap(1_000, vec![g1]), 1_000, 0).unwrap();
            assert_eq!(
                rec.schema_cache_stats(),
                SchemaCacheStats { hits: 0, misses: 1 }
            );

            // Tick 2: schema included AGAIN, same hash -> hit (not re-parsed).
            let g2 = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(2)],
                Some(Window::new(100, 200)),
                true,
            );
            rec.ingest(&v3_snap(1_100, vec![g2]), 1_100, 0).unwrap();
            assert_eq!(
                rec.schema_cache_stats(),
                SchemaCacheStats { hits: 1, misses: 1 }
            );

            // Tick 3: schema OMITTED (the producer's own cache hit) -> resolves
            // from the recorder's cache -> hit.
            let g3 = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(3)],
                Some(Window::new(200, 300)),
                false,
            );
            rec.ingest(&v3_snap(1_200, vec![g3]), 1_200, 0).unwrap();
            assert_eq!(
                rec.schema_cache_stats(),
                SchemaCacheStats { hits: 2, misses: 1 }
            );
            assert_eq!(
                rec.open_rows("cpu_usage/percpu"),
                3,
                "all three ticks (two hits, one miss) produced a row"
            );
        }

        #[test]
        fn a_schema_less_group_with_an_unresolved_hash_is_skipped() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            let sch = group_schema(&["0"]);
            // `schema: None` for a hash the cache has never seen — the
            // producer believes the recorder already has it; the recorder
            // does not, and must skip rather than fabricate a schema.
            let g = group_snapshot("cpu_usage/percpu", &sch, vec![Some(1)], None, false);
            rec.ingest(&v3_snap(1_000, vec![g]), 1_000, 0).unwrap();

            assert_eq!(rec.open_rows("cpu_usage/percpu"), 0);
            assert_eq!(rec.schema_cache_stats(), SchemaCacheStats::default());
        }

        #[test]
        fn segment_rotation_re_anchors_a_groups_schema_in_the_wal() {
            // The WAL-self-sufficiency property: even though the recorder's
            // OWN schema cache has known this group's schema since tick 1
            // (an ingest-side hit, not a miss), the WAL row after a rotation
            // must still carry the schema — a fresh reader process (or the
            // writer thread materializing THIS segment) has no access to that
            // cache and must be able to rebuild the table from the WAL alone.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, rid) = recorder(&path, seals_immediately());

            let sch = group_schema(&["0"]);
            let g1 = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(1)],
                Some(Window::new(0, 100)),
                true,
            );
            rec.ingest(&v3_snap(1_000, vec![g1]), 1_000, 0).unwrap();
            rec.maybe_seal().unwrap(); // rotates: segment_schema is cleared

            // Tick 2, same schema, sent WITHOUT it (the agent's own cache
            // hit) — the recorder must still re-anchor it in THIS segment's
            // WAL row, independent of what the agent sent.
            let g2 = group_snapshot(
                "cpu_usage/percpu",
                &sch,
                vec![Some(2)],
                Some(Window::new(100, 200)),
                false,
            );
            rec.ingest(&v3_snap(1_100, vec![g2]), 1_100, 0).unwrap();
            rec.sync().unwrap();

            let db = RezDb::open(&path).unwrap();
            let live = db.live_wal(rid, "cpu_usage/percpu").unwrap();
            assert_eq!(live.len(), 1, "the second tick is still live, unsealed");
            let decoded = decode_wal_group_row(&live[0].row).unwrap();
            assert!(
                decoded.schema.is_some(),
                "the first WAL row of a new segment must re-anchor the schema, \
                 even though the recorder's own cache already had a hit for it"
            );
        }

        #[test]
        fn table_sampler_selects_a_samplers_group_tables_out_of_a_mixed_recording() {
            // The manifest/filter unit: `filter --samplers cpu_usage` (once
            // that lands for V3 SQLite — see `parquet_tools::filter`'s
            // explicit "not yet supported" branch, unchanged by this build)
            // must select every `cpu_usage/*` group table and none of another
            // sampler's. Proven here against a REAL recording holding two
            // `cpu_usage` groups and one `blockio_requests` group, sealed
            // into actual catalog rows — not just the pure string-splitting
            // rule (`table_sampler_tests`, in `rez.rs`).
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, rid) = recorder(&path, seals_immediately());

            let sch = group_schema(&["0"]);
            let w = Some(Window::new(0, 100));
            let ts = 1_000u64;
            rec.ingest(
                &v3_snap(
                    ts,
                    vec![
                        group_snapshot("cpu_usage/percpu", &sch, vec![Some(1)], w, true),
                        group_snapshot("cpu_usage/aggregate", &sch, vec![Some(2)], w, true),
                        group_snapshot("blockio_requests/latency", &sch, vec![Some(3)], w, true),
                    ],
                ),
                ts,
                0,
            )
            .unwrap();
            rec.maybe_seal().unwrap();
            rec.sync().unwrap();

            let db = RezDb::open(&path).unwrap();
            let all = db.all_samplers(rid).unwrap();
            let mut selected: Vec<&str> = all
                .iter()
                .map(String::as_str)
                .filter(|t| crate::rez::table_sampler(t) == "cpu_usage")
                .collect();
            selected.sort();
            assert_eq!(selected, vec!["cpu_usage/aggregate", "cpu_usage/percpu"]);
        }

        // -----------------------------------------------------------------
        // Un-anchored WAL rows (Important 2): retention can delete a group's
        // anchor row out from under a still-live span. materialize_wal_tail
        // must degrade — skip what it cannot decode — not kill the writer.
        // -----------------------------------------------------------------

        #[test]
        fn materialize_wal_tail_skips_un_anchored_leading_rows() {
            let sch = group_schema(&["0"]);
            // Simulates retention having deleted this group's true anchor
            // row: the oldest surviving row has `schema: None` and there is
            // nothing before it in this span to resolve it against.
            let unanchored = WalRow {
                sampler: "cpu_usage/percpu".to_string(),
                ts: 1_000,
                wall_offset: 0,
                row: encode_wal_group_row(&WalGroupRow {
                    schema_hash: sch.hash(),
                    schema: None,
                    window: Some((900, 1_000)),
                    counters: vec![Some(1)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                })
                .unwrap(),
            };
            let anchored = WalRow {
                sampler: "cpu_usage/percpu".to_string(),
                ts: 2_000,
                wall_offset: 0,
                row: encode_wal_group_row(&WalGroupRow {
                    schema_hash: sch.hash(),
                    schema: Some((&sch).into()),
                    window: Some((1_900, 2_000)),
                    counters: vec![Some(2)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                })
                .unwrap(),
            };
            let tail = materialize_wal_tail("cpu_usage/percpu", &[unanchored, anchored])
                .expect("materialization must not error on an un-anchored leading row")
                .expect("the anchored row must still materialize");
            // THE catalog-accuracy fix: before it, a caller (`seal_batch`)
            // derived `SegmentMeta::rows`/`first_ts` from the raw WAL span
            // regardless of skips — here that would have been `rows: 2`,
            // `first_ts: 1_000` for a segment that actually holds ONE row
            // starting at `ts=2_000`. `MaterializedTail` now reports the
            // truth: what actually went into `bytes`.
            assert_eq!(
                (tail.rows, tail.first_ts),
                (1, 2_000),
                "the catalog extent must reflect the ONE row that materialized \
                 (ts=2_000), not the raw WAL span's rows=2/first_ts=1_000"
            );
            let table =
                crate::rez::read_table_parquet("cpu_usage/percpu".to_string(), tail.bytes).unwrap();
            assert_eq!(
                table.timestamps,
                vec![2_000],
                "the un-anchored row is skipped; the remainder materializes"
            );
        }

        #[test]
        fn materialize_wal_tail_of_a_fully_unanchored_span_is_none() {
            let sch = group_schema(&["0"]);
            let row = WalRow {
                sampler: "cpu_usage/percpu".to_string(),
                ts: 1_000,
                wall_offset: 0,
                row: encode_wal_group_row(&WalGroupRow {
                    schema_hash: sch.hash(),
                    schema: None,
                    window: None,
                    counters: vec![Some(1)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                })
                .unwrap(),
            };
            assert_eq!(
                materialize_wal_tail("cpu_usage/percpu", &[row]).unwrap(),
                None,
                "a span with nothing resolvable at all is the same as an empty tail"
            );
        }

        /// The end-to-end shape Important 2 actually describes: a hindsight
        /// buffer whose retention deletes an anchor row out from under a
        /// still-live group must keep sealing and stay readable, not die —
        /// exercised with a REAL `evict_before` (via `StreamRecorderV3`) and
        /// the exact rows it leaves behind, not a hand-built fixture.
        ///
        /// Three ticks, one schema change: tick 1 anchors schema A, tick 2
        /// repeats it (relying on tick 1's anchor, `schema: None`), tick 3
        /// changes to schema B — a fresh, self-carried anchor within the
        /// SAME (never-sealed) live span, per `ingest_v3`'s per-segment
        /// re-anchor rule (it fires on any hash change, not only rotation).
        /// Evicting through tick 1 leaves tick 2 genuinely unresolvable (its
        /// only anchor is gone) and tick 3 still resolvable (self-anchored)
        /// — exactly the "skip the leading un-anchored rows, materialize
        /// from the next anchor onward" shape, not the degenerate
        /// everything-lost case `materialize_wal_tail_of_a_fully_unanchored_span_is_none`
        /// covers.
        #[test]
        fn eviction_of_an_anchor_row_does_not_kill_the_writer() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, rid) = recorder(&path, never_seals());

            let sch_a = group_schema(&["0"]);
            let sch_b = group_schema(&["0", "1"]); // distinct arity -> distinct hash

            let g1 = group_snapshot(
                "cpu_usage/percpu",
                &sch_a,
                vec![Some(1)],
                Some(Window::new(900, 1_000)),
                true,
            );
            rec.ingest(&v3_snap(1_000, vec![g1]), 1_000, 0).unwrap();
            let g2 = group_snapshot(
                "cpu_usage/percpu",
                &sch_a,
                vec![Some(2)],
                Some(Window::new(1_000, 1_100)),
                false,
            );
            rec.ingest(&v3_snap(1_100, vec![g2]), 1_100, 0).unwrap();
            let g3 = group_snapshot(
                "cpu_usage/percpu",
                &sch_b,
                vec![Some(3), Some(4)],
                Some(Window::new(1_100, 1_200)),
                true,
            );
            rec.ingest(&v3_snap(1_200, vec![g3]), 1_200, 0).unwrap();
            rec.sync().unwrap();

            // Evict everything through tick 1 (schema A's ONLY anchor row) —
            // exactly what a short hindsight `duration` can do.
            rec.evict_before(1_001).unwrap();
            rec.sync().unwrap();

            let db = RezDb::open(&path).unwrap();
            let live = db.live_wal(rid, "cpu_usage/percpu").unwrap();
            assert_eq!(
                live.iter().map(|r| r.ts).collect::<Vec<_>>(),
                vec![1_100, 1_200],
                "tick 1's anchor row is gone; ticks 2 and 3 are still live"
            );

            // Materializing this span — exactly what `seal_batch` does with
            // whatever `live_wal` returns — must not error. The old behavior
            // (Err from materialize_group_wal_tail on tick 2's now-dangling
            // reference) would fail seal_batch inside its one transaction,
            // kill the writer thread, and take the whole recording down with
            // it.
            let tail = materialize_wal_tail("cpu_usage/percpu", &live)
                .expect("materializing a real, evicted-anchor live span must not error")
                .expect("tick 3's self-anchored row must still materialize");
            // The catalog-accuracy fix, against the real `live_wal` rows
            // `seal_batch` would have read: the raw span is `[1_100, 1_200]`
            // (2 rows), which is what `SegmentMeta` used to catalog
            // regardless of the skip — `rows: 2, first_ts: 1_100` for a
            // segment that actually holds ONE row starting at `ts=1_200`.
            assert_eq!(
                (tail.rows, tail.first_ts),
                (1, 1_200),
                "the catalog extent must reflect the ONE row that materialized \
                 (ts=1_200, tick 3's self-anchored row), not the raw live_wal \
                 span's rows=2/first_ts=1_100"
            );
            let table =
                crate::rez::read_table_parquet("cpu_usage/percpu".to_string(), tail.bytes).unwrap();
            assert_eq!(
                table.timestamps,
                vec![1_200],
                "tick 2 (unresolvable — its only anchor was evicted) is skipped; \
                 tick 3 (self-anchored) survives"
            );
        }

        // -----------------------------------------------------------------
        // Duplicate group names in one tick (Important 4): the recorder
        // accepts any msgpack endpoint, not just the rezolus agent, so a
        // producer-supplied duplicate name must degrade, not take the
        // recording down via a `(recording_id, sampler, ts)` PK violation.
        // -----------------------------------------------------------------

        #[test]
        fn duplicate_group_names_in_one_tick_do_not_kill_the_recording() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            let sch = group_schema(&["0"]);
            let w = Some(Window::new(0, 100));
            let first = group_snapshot("cpu_usage/percpu", &sch, vec![Some(1)], w, true);
            let second = group_snapshot("cpu_usage/percpu", &sch, vec![Some(2)], w, true);
            rec.ingest(&v3_snap(1_000, vec![first, second]), 1_000, 0)
                .unwrap();
            assert_eq!(
                rec.open_rows("cpu_usage/percpu"),
                1,
                "only the first occurrence of a name repeated in one tick is kept"
            );

            // Prove the writer is genuinely still alive, not just that this
            // one hand-off happened to return Ok — the old bug's PK
            // violation surfaces asynchronously, on a LATER hand-off, not
            // necessarily this one.
            for i in 1..=3u64 {
                let g = group_snapshot(
                    "cpu_usage/percpu",
                    &sch,
                    vec![Some(10 + i)],
                    Some(Window::new(1_000 + i * 100, 1_000 + i * 100 + 50)),
                    false,
                );
                rec.ingest(&v3_snap(2_000 + i, vec![g]), 2_000 + i, 0)
                    .unwrap();
            }
            rec.sync()
                .expect("the writer must still be accepting ticks after a duplicate group name");
            assert_eq!(
                rec.open_rows("cpu_usage/percpu"),
                4,
                "the deduplicated first tick plus the three that followed"
            );
        }

        // -----------------------------------------------------------------
        // Schema cache bound (Important 1): the cache must not grow without
        // bound as a group's schema churns (e.g. cgroup add/remove).
        // -----------------------------------------------------------------

        #[test]
        fn the_schema_cache_stays_bounded_per_name() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("out.rez");
            let (_archive, mut rec, _rid) = recorder(&path, never_seals());

            // SCHEMA_RING_LEN + 1 distinct schemas for the SAME group name —
            // each with a different member count, so each hashes distinctly.
            let member_names: Vec<String> = (0..(SCHEMA_RING_LEN + 1))
                .map(|n| format!("m{n}"))
                .collect();
            let schemas: Vec<GroupSchema> = (1..=(SCHEMA_RING_LEN + 1))
                .map(|n| {
                    group_schema(
                        &member_names[..n]
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();

            for (i, sch) in schemas.iter().enumerate() {
                let counters = vec![Some(1); sch.counters.len()];
                let g = group_snapshot(
                    "cpu_usage/percpu",
                    sch,
                    counters,
                    Some(Window::new(i as u64 * 100, i as u64 * 100 + 50)),
                    true,
                );
                rec.ingest(&v3_snap(1_000 + i as u64, vec![g]), 1_000 + i as u64, 0)
                    .unwrap();
            }
            assert_eq!(
                rec.schema_cache_stats(),
                SchemaCacheStats {
                    hits: 0,
                    misses: (SCHEMA_RING_LEN + 1) as u64
                },
                "every schema here is genuinely new"
            );
            let rows_before = rec.open_rows("cpu_usage/percpu");

            // The FIRST schema has now aged out of the bounded ring (only
            // the newest SCHEMA_RING_LEN generations are kept) — a
            // `schema: None` payload referencing it must be treated as
            // unresolved, not silently served from an unbounded cache.
            let stale = group_snapshot(
                "cpu_usage/percpu",
                &schemas[0],
                vec![Some(1); schemas[0].counters.len()],
                Some(Window::new(9_000, 9_050)),
                false,
            );
            rec.ingest(&v3_snap(2_000, vec![stale]), 2_000, 0).unwrap();
            assert_eq!(
                rec.open_rows("cpu_usage/percpu"),
                rows_before,
                "a schema evicted from the ring must not produce a row"
            );
        }
    }
}
