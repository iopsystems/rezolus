//! Streaming `.rez` writer thread. See docs/journal/2026-08-11-rez-streaming-writer.md.
//!
//! Two halves: `RezWriterHandle` owns a dedicated writer thread that encodes
//! sealed segments to parquet and appends them to the output tar, and
//! `StreamRecorder` owns the scrape-side per-sampler builders and decides when
//! a segment is due. Sealing runs off the scrape loop so a large parquet encode
//! cannot skew the sampling cadence; the channel is bounded, so a disk that
//! cannot keep up backpressures the loop instead of growing memory.
//!
//! Contract: PANIC-FREE — every fallible op returns `Err`. The global panic
//! hook (`src/main.rs:57-62`) prints and calls `process::exit(101)` BEFORE
//! unwinding, so a panic here never reaches the send-error path, skips
//! finalize, and in wrapped mode orphans the child.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use metriken_exposition::Snapshot;
use tracing::warn;

use super::rez::{
    append_tar_entry, dedup_key, group_by_sampler, write_table_parquet, RezManifest, RezRecording,
    RezTable, RezTableIndex, TableBuilder, REZ_MANIFEST_NAME, REZ_SCHEMA_VERSION,
};

/// The fixed parts of the recording's manifest entry, known at recording start.
pub(crate) struct ManifestSeed {
    /// Directory holding this recording's tables inside the tar.
    pub dir: String,
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    /// Wall-clock reading (ns since epoch) at recording start. Row timestamps
    /// are `anchor + monotonic elapsed`, so this pins the timeline to wall time.
    pub clock_anchor_wall_ns: u64,
}

/// One sealed segment handed to the writer thread. The table already carries
/// its `wall_offsets`; everything here is owned data (`Send + 'static`).
pub(crate) struct SealJob {
    pub sampler: String,
    pub table: RezTable,
}

enum WriterMsg {
    /// One seal batch = one checkpoint: all due segments, then one manifest.
    Seal(Vec<SealJob>),
    /// Final partial segments plus the loop's last clock observation.
    Finalize {
        tails: Vec<SealJob>,
        clock_offset: (u64, i64),
    },
}

/// Per-table running totals. Checkpoint manifests describe *exactly* the
/// segments they reference — sealed rows only, never open builders — so a
/// recovered archive never over-reports recoverable data.
#[derive(Default)]
struct TableState {
    /// Segment file names relative to the recording dir, in segment order.
    files: Vec<String>,
    rows: u64,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
    /// Union of the metric column names seen across segments, in first-seen
    /// order. Only the FINAL manifest carries it: `columns` is O(total columns)
    /// and reaches 100 KB+ on cgroup-heavy tables, and recovery does not need
    /// it to load segments.
    columns: Vec<String>,
    seen_columns: HashSet<String>,
}

impl TableState {
    fn index(&self, sampler: &str, with_columns: bool) -> RezTableIndex {
        RezTableIndex {
            sampler: sampler.to_string(),
            // Set iff the table is a single segment, matching
            // `write_archive_bytes`' file-iff-single rule so a one-segment
            // streaming table is still openable by a v1 reader. Only that rule
            // is shared: the names differ (the atomic writer's single segment is
            // `<sampler>.parquet`, a streamed one is `<sampler>/0000.parquet`,
            // because segment count is not knowable at seal time). Either way
            // this is a truthful alias, never a pointer at partial data.
            file: match self.files.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            },
            files: self.files.clone(),
            columns: if with_columns {
                self.columns.clone()
            } else {
                Vec::new()
            },
            rows: self.rows,
            // What `cadence_hint` would report for the concatenated table: the
            // mean row interval across every sealed segment, `None` under 2
            // rows. Computed from the running first/last stamps because the
            // segments themselves are long gone by now.
            cadence_ns: match (self.first_ts, self.last_ts) {
                (Some(first), Some(last)) if self.rows >= 2 => {
                    Some(last.saturating_sub(first) / (self.rows - 1))
                }
                _ => None,
            },
        }
    }
}

/// Handle to the writer thread. Every fallible hand-off reports the writer's
/// stored error, in the required order: send-failure → join → report.
pub(crate) struct RezWriterHandle {
    tx: Option<SyncSender<WriterMsg>>,
    thread: Option<JoinHandle<Result<(), String>>>,
    partial: PathBuf,
}

impl RezWriterHandle {
    /// Create `<output>.partial`, write the initial empty checkpoint manifest,
    /// and spawn the writer thread.
    pub(crate) fn create(output: &Path, seed: ManifestSeed) -> Result<Self, String> {
        let partial = partial_path(output)?;
        rename_aside_if_present(&partial)?;

        // O_EXCL, so a racing creator in the rename→create window is a clear
        // error rather than a silent shared file. (A writer that already holds
        // the path cannot be detected this way — rename-aside ran first — so
        // two recorders pointed at one output produce two archives, not a
        // corrupt one.) The output path itself is never opened, so a
        // pre-existing output file is never truncated at t=0.
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|e| format!("failed to create {}: {e}", partial.display()))?;

        // Past the create, a failure must not leave the `.partial` behind: it
        // holds no recoverable data, but the next run cannot know that and
        // would rename it aside as `.recovered-<n>`, accumulating garbage.
        Self::start(file, &partial, output, seed).inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })
    }

    /// Everything between "the `.partial` exists" and "the writer thread owns
    /// it". Split out so `create` can unlink the file if any of it fails.
    fn start(
        file: File,
        partial: &Path,
        output: &Path,
        seed: ManifestSeed,
    ) -> Result<Self, String> {
        // Without this the dirent itself can be lost to a power cut, taking the
        // whole recovery artifact with it.
        sync_parent_dir(partial)?;

        // The tar `File` stays UNBUFFERED on purpose: the `tar` crate writes
        // straight through, and a `BufWriter` would leave entry bytes in
        // user space where no `sync_data` can reach them.
        let mut builder = tar::Builder::new(file);
        builder.mode(tar::HeaderMode::Deterministic);

        // The initial empty checkpoint manifest is the FIRST tar entry: without
        // it nothing identifies the file as `.rez` until the first seal batch,
        // so an in-progress or early-killed recording would sniff as not-`.rez`
        // and be misrouted to the parquet path by every dispatcher.
        let tables = BTreeMap::new();
        let manifest = build_manifest(&seed, &tables, &[], false, false)?;
        append_manifest(&mut builder, &manifest)?;
        sync(&mut builder)?;

        let (tx, rx) = sync_channel(1);
        let output = output.to_path_buf();
        let thread_partial = partial.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("rez-writer".to_string())
            .spawn(move || writer_thread(rx, builder, seed, thread_partial, output))
            .map_err(|e| format!("failed to spawn the .rez writer thread: {e}"))?;

        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            partial: partial.to_path_buf(),
        })
    }

    /// The in-progress archive's path — the recovery artifact if this recording
    /// never finalizes.
    pub(crate) fn partial_path(&self) -> &Path {
        &self.partial
    }

    /// Hand one seal batch (= one checkpoint) to the writer. Blocks while the
    /// channel is full: that is the intended backpressure signal.
    pub(crate) fn seal(&mut self, batch: Vec<SealJob>) -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        self.send(WriterMsg::Seal(batch))
    }

    /// Seal the tails, write the final manifest and tar footer, rename the
    /// `.partial` into place.
    pub(crate) fn finalize(
        mut self,
        tails: Vec<SealJob>,
        clock_offset: (u64, i64),
    ) -> Result<(), String> {
        self.send(WriterMsg::Finalize {
            tails,
            clock_offset,
        })?;
        self.join()
    }

    /// Give up on the recording: stop the writer and unlink the `.partial`.
    pub(crate) fn abort(mut self) {
        if let Err(e) = self.join() {
            warn!("the .rez writer failed before the recording was aborted: {e}");
        }
        // Best effort: an unlink failure leaves a stray `.partial`, nothing worse.
        let _ = std::fs::remove_file(&self.partial);
    }

    fn send(&mut self, msg: WriterMsg) -> Result<(), String> {
        let Some(tx) = self.tx.as_ref() else {
            return Err("the .rez writer thread has already been joined".to_string());
        };
        if tx.send(msg).is_ok() {
            return Ok(());
        }
        // The receiver is gone, so the writer has exited (it exits its receive
        // loop on the first error). Send-failure → join → report the stored
        // error, rather than logging per-tick against a corrupt archive.
        Err(self.join().err().unwrap_or_else(|| {
            "the .rez writer thread exited before the recording finished".to_string()
        }))
    }

    /// Close the channel and join the writer, returning its stored result.
    /// Idempotent: a second call is a no-op `Ok`.
    fn join(&mut self) -> Result<(), String> {
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
}

impl Drop for RezWriterHandle {
    /// The writer must be joined on every path out — including the ones that
    /// skip an explicit `finalize`/`abort` — so a dropped handle never leaves a
    /// detached thread still appending to the archive.
    fn drop(&mut self) {
        if let Err(e) = self.join() {
            warn!("the .rez writer failed: {e}");
        }
    }
}

/// `<output>` with `.partial` appended to the file name (`out.rez` →
/// `out.rez.partial`), not extension-replaced.
fn partial_path(output: &Path) -> Result<PathBuf, String> {
    let name = output
        .file_name()
        .ok_or_else(|| format!("{} is not a file path", output.display()))?;
    let mut name = name.to_os_string();
    name.push(".partial");
    Ok(output.with_file_name(name))
}

/// A leftover `.partial` may hold recoverable data from a previous crash, so it
/// is renamed to `<partial>.recovered-<n>` (first free `n`) with a warning —
/// never clobbered.
fn rename_aside_if_present(partial: &Path) -> Result<(), String> {
    if !partial.exists() {
        return Ok(());
    }
    for n in 0u32.. {
        let mut name = partial
            .file_name()
            .ok_or_else(|| format!("{} is not a file path", partial.display()))?
            .to_os_string();
        name.push(format!(".recovered-{n}"));
        let aside = partial.with_file_name(name);
        if aside.exists() {
            continue;
        }
        std::fs::rename(partial, &aside).map_err(|e| {
            format!(
                "failed to move the existing {} aside to {}: {e}",
                partial.display(),
                aside.display()
            )
        })?;
        warn!(
            "found an unfinished recording at {}; it may hold recoverable data and was moved to {}",
            partial.display(),
            aside.display()
        );
        return Ok(());
    }
    Err(format!(
        "no free .recovered-<n> name beside {}",
        partial.display()
    ))
}

fn sync_parent_dir(path: &Path) -> Result<(), String> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| format!("failed to sync directory {}: {e}", parent.display()))
}

fn sync(builder: &mut tar::Builder<File>) -> Result<(), String> {
    builder
        .get_mut()
        .sync_data()
        .map_err(|e| format!("failed to sync the .rez archive: {e}"))
}

fn append_manifest(builder: &mut tar::Builder<File>, manifest: &RezManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| format!("failed to serialize the .rez manifest: {e}"))?;
    append_tar_entry(builder, REZ_MANIFEST_NAME, &bytes)
        .map_err(|e| format!("failed to write the .rez manifest: {e}"))
}

fn build_manifest(
    seed: &ManifestSeed,
    tables: &BTreeMap<String, TableState>,
    clock_offsets: &[(u64, i64)],
    complete: bool,
    with_columns: bool,
) -> Result<RezManifest, String> {
    Ok(RezManifest {
        version: REZ_SCHEMA_VERSION,
        recordings: vec![RezRecording {
            dir: seed.dir.clone(),
            labels: seed.labels.clone(),
            metadata: seed.metadata.clone(),
            complete,
            clock_anchor_wall_ns: Some(seed.clock_anchor_wall_ns),
            clock_offsets: clock_offsets.to_vec(),
            tables: tables
                .iter()
                .map(|(sampler, state)| state.index(sampler, with_columns))
                .collect(),
        }],
    })
}

/// Encode and append one batch's segments, updating the running totals.
///
/// Returns the batch's clock observation — the newest sealed row's
/// `(timestamp, wall_offset)` — for the recording's `clock_offsets` series.
/// Deriving it from the rows just sealed keeps the summary consistent with the
/// `:wall_offset` column it summarizes, with no extra plumbing.
fn seal_segments(
    builder: &mut tar::Builder<File>,
    dir: &str,
    tables: &mut BTreeMap<String, TableState>,
    batch: Vec<SealJob>,
) -> Result<Option<(u64, i64)>, String> {
    let mut observation: Option<(u64, i64)> = None;
    for job in batch {
        let bytes = write_table_parquet(&job.table)
            .map_err(|e| format!("failed to encode a {} segment: {e}", job.sampler))?;
        let state = tables.entry(job.sampler.clone()).or_default();
        let name = format!("{}/{:04}.parquet", job.sampler, state.files.len());
        append_tar_entry(builder, &format!("{dir}/{name}"), &bytes)
            .map_err(|e| format!("failed to write a {} segment: {e}", job.sampler))?;

        state.files.push(name);
        state.rows += job.table.timestamps.len() as u64;
        if let Some(&first) = job.table.timestamps.first() {
            state.first_ts.get_or_insert(first);
        }
        if let Some(&last) = job.table.timestamps.last() {
            state.last_ts = Some(last);
        }
        for col in &job.table.columns {
            if state.seen_columns.insert(col.name.clone()) {
                state.columns.push(col.name.clone());
            }
        }

        let last_row = job
            .table
            .timestamps
            .last()
            .map(|&ts| (ts, job.table.wall_offsets.last().copied().unwrap_or(0)));
        if let Some((ts, offset)) = last_row {
            if observation.is_none_or(|(seen, _)| ts >= seen) {
                observation = Some((ts, offset));
            }
        }
    }
    Ok(observation)
}

/// The writer thread body. Every fallible operation returns `Err`; the loop
/// exits on the first error so the failure surfaces on the next hand-off
/// instead of accumulating against a corrupt archive.
fn writer_thread(
    rx: Receiver<WriterMsg>,
    mut builder: tar::Builder<File>,
    seed: ManifestSeed,
    partial: PathBuf,
    output: PathBuf,
) -> Result<(), String> {
    let mut tables: BTreeMap<String, TableState> = BTreeMap::new();
    let mut clock_offsets: Vec<(u64, i64)> = Vec::new();

    loop {
        match rx.recv() {
            Ok(WriterMsg::Seal(batch)) => {
                if let Some(observation) =
                    seal_segments(&mut builder, &seed.dir, &mut tables, batch)?
                {
                    clock_offsets.push(observation);
                }
                // Two syncs, and the order is load-bearing: write order is NOT
                // persistence order, so a single post-manifest sync can persist
                // a manifest referencing segment data that is not durable yet,
                // leaving recovery pointing at garbage. With this order, any
                // persisted manifest byte implies durable segments (a
                // half-persisted manifest fails to parse and recovery falls
                // back to the previous checkpoint).
                sync(&mut builder)?;
                let manifest = build_manifest(&seed, &tables, &clock_offsets, false, false)?;
                append_manifest(&mut builder, &manifest)?;
                sync(&mut builder)?;
            }
            Ok(WriterMsg::Finalize {
                tails,
                clock_offset,
            }) => {
                // The tail batch contributes its observation exactly like any
                // other batch, so every entry in the series has one derivation:
                // the newest sealed row's `:wall_offset`.
                if let Some(observation) =
                    seal_segments(&mut builder, &seed.dir, &mut tables, tails)?
                {
                    clock_offsets.push(observation);
                }
                // The loop's final tick observation joins the series only when
                // it adds a timestamp no sealed row already covers — otherwise
                // the series would carry two conflicting offsets at one
                // timestamp and consumers could not read it uniformly. The
                // row-derived value wins because it is a projection of the
                // `:wall_offset` column; the tick value is kept when it is the
                // only sample covering the span after the last sealed row.
                if !clock_offsets.iter().any(|&(ts, _)| ts == clock_offset.0) {
                    clock_offsets.push(clock_offset);
                }
                // Same two-sync order, with the final manifest playing the role
                // of the checkpoint: segments durable, then the manifest that
                // names them, then the footer.
                sync(&mut builder)?;
                let manifest = build_manifest(&seed, &tables, &clock_offsets, true, true)?;
                append_manifest(&mut builder, &manifest)?;
                let file = builder
                    .into_inner()
                    .map_err(|e| format!("failed to finish the .rez archive: {e}"))?;
                file.sync_data()
                    .map_err(|e| format!("failed to sync the .rez archive: {e}"))?;
                drop(file);
                std::fs::rename(&partial, &output).map_err(|e| {
                    format!(
                        "failed to rename {} to {}: {e}",
                        partial.display(),
                        output.display()
                    )
                })?;
                // Make the rename itself durable.
                sync_parent_dir(&output)?;
                return Ok(());
            }
            // The handle was dropped without finalizing: leave the `.partial`
            // as the recovery artifact, readable up to its last checkpoint.
            Err(_) => return Ok(()),
        }
    }
}

/// When an open segment is due to be sealed. Byte-first: the byte cap is the
/// one that bounds both the builder's memory footprint and the encoder's input,
/// and it is maintained O(1) per entry by `TableBuilder::push_row`.
///
/// **The age bound exists for the kill-loss window, not finalize cost.** The
/// byte and row caps alone bound finalize time and memory — a slow sampler's
/// open segment is naturally tiny — so age sealing only bounds how much data an
/// unclean kill loses. It is also what drives segment count, so the trade (loss
/// window vs segments and read-time merge width) is deliberate.
pub(crate) struct SealPolicy {
    pub max_bytes: usize,
    pub max_rows: usize,
    pub max_age: Duration,
}

impl Default for SealPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_rows: 4096,
            max_age: Duration::from_secs(300),
        }
    }
}

/// The scrape-side half: per-sampler open segments plus the seal decision.
pub(crate) struct StreamRecorder {
    /// Open builder per sampler, with the instant it was opened (the age bound).
    builders: BTreeMap<String, (TableBuilder, Instant)>,
    /// Window-advance dedup keys. Held here, not on the builder, so dedup
    /// survives a builder rotation: the key of a row in an already-sealed
    /// segment must still suppress a re-observation.
    last_keys: BTreeMap<String, u64>,
    handle: RezWriterHandle,
    policy: SealPolicy,
}

impl StreamRecorder {
    pub(crate) fn new(handle: RezWriterHandle) -> Self {
        Self::with_policy(handle, SealPolicy::default())
    }

    pub(crate) fn with_policy(handle: RezWriterHandle, policy: SealPolicy) -> Self {
        Self {
            builders: BTreeMap::new(),
            last_keys: BTreeMap::new(),
            handle,
            policy,
        }
    }

    /// Append one scraped snapshot: partition by sampler and, for each sampler
    /// whose representative acquisition window advanced, push a row stamped
    /// `anchored_ts` with this tick's `wall_offset_ns` observation.
    pub(crate) fn ingest(&mut self, snapshot: &Snapshot, anchored_ts: u64, wall_offset_ns: i64) {
        for (sampler, entries) in group_by_sampler(snapshot) {
            let key = dedup_key(&entries, anchored_ts);
            if let Some(&last) = self.last_keys.get(sampler) {
                if key <= last {
                    continue; // window unchanged → same observation → skip
                }
            }
            self.last_keys.insert(sampler.to_string(), key);
            let (builder, _) = self
                .builders
                .entry(sampler.to_string())
                .or_insert_with(|| (TableBuilder::new(sampler.to_string()), Instant::now()));
            builder.push_row(anchored_ts, wall_offset_ns, &entries);
        }
    }

    /// Seal every open segment past any threshold, as ONE batch → one
    /// checkpoint. Empty builders never seal.
    ///
    /// Call this every loop iteration, scrape or not: an unreachable endpoint
    /// must still get its pre-outage rows sealed, or the kill-loss window is no
    /// longer bounded in time.
    pub(crate) fn maybe_seal(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let mut batch = Vec::new();
        for (sampler, (builder, opened_at)) in self.builders.iter_mut() {
            let rows = builder.rows();
            if rows == 0 {
                continue;
            }
            let due = builder.approx_bytes() >= self.policy.max_bytes
                || rows >= self.policy.max_rows
                || now.duration_since(*opened_at) >= self.policy.max_age;
            if !due {
                continue;
            }
            // Rotation is a `mem::replace`: the dedup key lives in `last_keys`
            // and carries over untouched, while the byte counter correctly
            // restarts at zero with the fresh segment.
            let sealed = std::mem::replace(builder, TableBuilder::new(sampler.clone()));
            *opened_at = now;
            batch.push(SealJob {
                sampler: sampler.clone(),
                table: sealed.finish(),
            });
        }
        self.handle.seal(batch)
    }

    /// Seal the remaining partial segments (small by construction) and finalize
    /// the archive.
    pub(crate) fn finalize(mut self, clock_offset: (u64, i64)) -> Result<(), String> {
        let tails: Vec<SealJob> = std::mem::take(&mut self.builders)
            .into_iter()
            .filter(|(_, (builder, _))| builder.rows() > 0)
            .map(|(sampler, (builder, _))| SealJob {
                sampler,
                table: builder.finish(),
            })
            .collect();
        self.handle.finalize(tails, clock_offset)
    }

    /// Give up on the recording: stop the writer and unlink the `.partial`.
    pub(crate) fn abort(self) {
        self.handle.abort();
    }

    /// The in-progress archive's path — the recovery artifact if this recording
    /// never finalizes.
    pub(crate) fn partial_path(&self) -> &Path {
        self.handle.partial_path()
    }

    /// Rows in a sampler's open (unsealed) segment.
    #[cfg(test)]
    fn open_rows(&self, sampler: &str) -> usize {
        self.builders
            .get(sampler)
            .map(|(builder, _)| builder.rows())
            .unwrap_or(0)
    }
}

/// Test support for the `.rez` consumers (reader, `parquet` tools): write a
/// genuinely **segmented** archive, which only the streaming writer produces.
///
/// Each of `samplers` gets `rows` one-second rows of a counter named
/// `<sampler>_ops`, sealed every `max_rows` rows → `ceil(rows / max_rows)`
/// segments per table. `finalize` chooses the outcome: `true` writes `out` (a
/// cleanly finalized, `complete` recording), `false` drops the writer and
/// leaves the recoverable `<out>.partial` (an incomplete recording). Returns
/// the path that now exists.
#[cfg(test)]
pub(crate) fn write_segmented_rez(
    out: &Path,
    dir: &str,
    labels: BTreeMap<String, String>,
    samplers: &[&str],
    rows: u64,
    max_rows: usize,
    finalize: bool,
) -> PathBuf {
    use crate::recorder::rez::recorder_tests_support::{counter, snap};
    use metriken::Window;

    let seed = ManifestSeed {
        dir: dir.to_string(),
        labels,
        metadata: [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect(),
        clock_anchor_wall_ns: 1_700_000_000_000_000_000,
    };
    let handle = RezWriterHandle::create(out, seed).unwrap();
    let partial = handle.partial_path().to_path_buf();
    let mut rec = StreamRecorder::with_policy(
        handle,
        SealPolicy {
            max_bytes: usize::MAX,
            max_rows,
            max_age: Duration::from_secs(3600),
        },
    );
    let mut last_ts = 0;
    for i in 0..rows {
        // Seconds-scale stamps with a window that advances every poll, so no
        // row is deduped and every sampler grows at the same rate.
        let ts = 1_000_000_000 * (i + 1);
        let w = Some(Window::new(ts - 50_000_000, ts));
        let counters = samplers
            .iter()
            .map(|s| counter(&format!("{s}_ops"), s, i * 1_000, w))
            .collect();
        rec.ingest(&snap(ts, counters), ts, 0);
        rec.maybe_seal().unwrap();
        last_ts = ts;
    }
    if finalize {
        rec.finalize((last_ts, 0)).unwrap();
        out.to_path_buf()
    } else {
        drop(rec);
        partial
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::{counter, snap};
    use crate::recorder::rez::{Entry, RezManifest, TableBuilder, REZ_MANIFEST_NAME};
    use metriken::Window;
    use std::io::Read;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;

    fn seed() -> ManifestSeed {
        ManifestSeed {
            dir: "rezolus".to_string(),
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "100".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        }
    }

    /// A sealable job with `ts.len()` rows for `sampler`, every row carrying
    /// `wall_offset` in the `:wall_offset` sidecar.
    fn job_with_offset(sampler: &str, ts: &[u64], wall_offset: i64) -> SealJob {
        let mut b = TableBuilder::new(sampler.to_string());
        for (i, &t) in ts.iter().enumerate() {
            let c = counter("0", sampler, i as u64, Some(Window::new(t - 100, t)));
            b.push_row(t, wall_offset, &[Entry::Counter(&c)]);
        }
        SealJob {
            sampler: sampler.to_string(),
            table: b.finish(),
        }
    }

    fn job(sampler: &str, ts: &[u64]) -> SealJob {
        job_with_offset(sampler, ts, 7)
    }

    /// A job the writer cannot encode: `wall_offsets` shorter than `timestamps`
    /// fails `RecordBatch`'s equal-length check inside `write_table_parquet`,
    /// which is the same shape of mid-recording writer failure as an ENOSPC tar
    /// append — it happens on the writer thread, after the hand-off returned.
    fn unencodable_job(sampler: &str) -> SealJob {
        SealJob {
            sampler: sampler.to_string(),
            table: RezTable {
                sampler: sampler.to_string(),
                timestamps: vec![1_000, 2_000],
                wall_offsets: vec![0],
                columns: Vec::new(),
            },
        }
    }

    /// Every `manifest.json` entry in the archive, oldest first — the production
    /// reader only surfaces the newest resolvable one, but the checkpoint
    /// history is exactly what these tests are about.
    fn all_manifests(path: &std::path::Path) -> Vec<RezManifest> {
        let mut out = Vec::new();
        let mut archive = tar::Archive::new(std::fs::File::open(path).unwrap());
        for entry in archive.entries().unwrap() {
            let Ok(mut entry) = entry else { break };
            let size = entry.size();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut buf = Vec::new();
            match entry.read_to_end(&mut buf) {
                Ok(read) if read as u64 == size => {}
                _ => break,
            }
            if name == REZ_MANIFEST_NAME {
                out.push(serde_json::from_slice(&buf).unwrap());
            }
        }
        out
    }

    fn partial_of(output: &std::path::Path) -> std::path::PathBuf {
        let mut name = output.file_name().unwrap().to_os_string();
        name.push(".partial");
        output.with_file_name(name)
    }

    // The initial empty checkpoint manifest is what makes an in-progress (or
    // early-killed) recording identifiable as `.rez` at all: without it nothing
    // sniffs as `.rez` until the first seal batch.
    #[test]
    fn create_writes_initial_manifest_and_partial() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        assert!(partial.exists(), "the .partial is created up front");
        assert!(!out.exists(), "the output appears only on clean finalize");
        assert!(crate::recorder::rez::is_rez_path(&partial).unwrap());

        let (manifest, recordings) = crate::recorder::rez::read_archive_bytes(&partial).unwrap();
        assert_eq!(manifest.version, 2);
        assert_eq!(manifest.recordings.len(), 1);
        let rec = &manifest.recordings[0];
        assert!(rec.tables.is_empty(), "no segments yet");
        assert!(!rec.complete, "an in-progress recording is never complete");
        assert_eq!(rec.clock_anchor_wall_ns, Some(ANCHOR));
        assert_eq!(
            rec.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
        assert_eq!(recordings.len(), 1);
        assert!(recordings[0].tables.is_empty());

        handle.abort();
    }

    #[test]
    fn seal_finalize_roundtrip_with_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle
            .seal(vec![job("cpu_usage", &[1_000, 2_000, 3_000])])
            .unwrap();
        handle
            .seal(vec![job("cpu_usage", &[4_000, 5_000])])
            .unwrap();
        handle.finalize(Vec::new(), (5_000, 11)).unwrap();

        assert!(
            out.exists(),
            "clean finalize renames the .partial into place"
        );
        assert!(!partial.exists());

        let manifests = all_manifests(&out);
        assert_eq!(
            manifests.len(),
            4,
            "initial + one checkpoint per seal batch + final"
        );
        for (i, m) in manifests.iter().enumerate().take(3) {
            assert!(!m.recordings[0].complete, "checkpoint {i} is not complete");
            for idx in &m.recordings[0].tables {
                assert!(
                    idx.columns.is_empty(),
                    "checkpoint {i} must omit the columns list"
                );
            }
        }
        // Checkpoints describe exactly the segments they reference.
        assert_eq!(manifests[1].recordings[0].tables[0].rows, 3);
        assert_eq!(manifests[2].recordings[0].tables[0].rows, 5);

        let (manifest, recordings) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        assert_eq!(manifest.version, 2);
        let rec = &manifest.recordings[0];
        assert!(rec.complete, "finalize marks the recording complete");
        assert_eq!(rec.tables.len(), 1);
        let idx = &rec.tables[0];
        assert_eq!(idx.sampler, "cpu_usage");
        assert_eq!(
            idx.files,
            vec!["cpu_usage/0000.parquet", "cpu_usage/0001.parquet"]
        );
        assert_eq!(idx.rows, 5);
        assert_eq!(idx.columns, vec!["0".to_string()]);
        assert_eq!(idx.cadence_ns, Some(1_000));
        // One entry per seal batch, each derived from that batch's newest
        // sealed row. The finalize-supplied (5_000, 11) is dropped: ts 5_000 is
        // already covered by a row-derived observation, and two conflicting
        // offsets at one timestamp would make the series unreadable.
        assert_eq!(rec.clock_offsets, vec![(3_000, 7), (5_000, 7)]);

        // The bytes are readable by the truncation-tolerant reader.
        assert_eq!(recordings[0].tables.len(), 1);
        assert_eq!(recordings[0].tables[0].1.len(), 2, "two segment blobs");
        assert!(recordings[0].complete);
    }

    // The checkpoint clock observation is the newest sealed row in the batch
    // paired with *that same table's* offset — not the last job's, not the
    // oldest row's, and never a cross-table mix of timestamp and offset.
    #[test]
    fn checkpoint_clock_offset_is_the_newest_sealed_rows_observation() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle
            .seal(vec![
                // The newest row is in the FIRST job, and the older sampler
                // carries a wildly different offset.
                job_with_offset("cpu_usage", &[3_000], 7),
                job_with_offset("scheduler", &[1_000, 2_000], 99),
            ])
            .unwrap();
        handle.finalize(Vec::new(), (9_000, -5)).unwrap();

        let manifests = all_manifests(&out);
        assert_eq!(
            manifests[1].recordings[0].clock_offsets,
            vec![(3_000, 7)],
            "max timestamp across the batch, paired with its own table's offset"
        );
        // The finalize tick observation covers a span no sealed row does, so it
        // joins the series.
        assert_eq!(
            manifests[2].recordings[0].clock_offsets,
            vec![(3_000, 7), (9_000, -5)]
        );
    }

    // A writer-thread failure must surface on a hand-off as the writer's own
    // error — this is what decides whether a corrupt archive gets noticed
    // instead of producing per-tick log spam.
    #[test]
    fn writer_error_surfaces_on_the_next_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        // The hand-off itself succeeds: the failure happens on the writer.
        handle.seal(vec![unencodable_job("cpu_usage")]).unwrap();

        // The next send that finds the receiver gone joins and reports. A
        // bounded retry because the channel buffers one message, so the first
        // send after the failure may still be accepted.
        let mut surfaced = None;
        for _ in 0..500 {
            match handle.seal(vec![job("scheduler", &[1_000])]) {
                Ok(()) => std::thread::sleep(Duration::from_millis(1)),
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
        // reached the archive, and the output was never produced.
        let (manifest, _) = crate::recorder::rez::read_archive_bytes(&partial).unwrap();
        assert!(
            manifest.recordings[0].tables.is_empty(),
            "the writer stopped at the first error"
        );
        assert!(!out.exists());
    }

    #[test]
    fn writer_error_surfaces_through_finalize() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle.seal(vec![unencodable_job("cpu_usage")]).unwrap();
        // Whether the send lands or fails, finalize joins and reports the
        // writer's stored error rather than claiming success.
        let err = handle
            .finalize(Vec::new(), (1_000, 0))
            .expect_err("finalize must report the writer's error");
        assert!(
            err.contains("failed to encode a cpu_usage segment"),
            "got: {err}"
        );
        assert!(
            !out.exists(),
            "a failed recording is never renamed into place"
        );
    }

    // Real SIGKILL/power-loss geometry: truncate bytes this writer actually
    // produced (the hand-built tars in `rez.rs::recovery_tests` cover the reader
    // rules, not the writer's own output).
    #[test]
    fn truncated_writer_bytes_recover_the_sealed_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle
            .seal(vec![job("cpu_usage", &[1_000, 2_000])])
            .unwrap();
        handle.seal(vec![job("cpu_usage", &[3_000])]).unwrap();
        handle
            .seal(vec![job("cpu_usage", &[4_000]), job("scheduler", &[4_000])])
            .unwrap();
        drop(handle);
        let bytes = std::fs::read(&partial).unwrap();

        // A coarse sweep plus every interesting geometry: each entry's header
        // start (chop mid-header / at a block boundary) and the middle of each
        // entry's data (chop mid-segment-data and mid-manifest). Cutting at the
        // last manifest's header start is the "killed between a segment append
        // and its checkpoint" case.
        let mut cuts: Vec<usize> = (0..bytes.len()).step_by(64).collect();
        let mut archive = tar::Archive::new(std::io::Cursor::new(bytes.clone()));
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let head = entry.raw_header_position() as usize;
            cuts.push(head);
            cuts.push(head + 512);
            cuts.push(head + 512 + entry.size() as usize / 2);
        }
        cuts.push(bytes.len());
        cuts.retain(|&c| c <= bytes.len());
        cuts.sort_unstable();
        cuts.dedup();

        let cut_path = dir.path().join("cut.rez");
        let mut first_ok: Option<usize> = None;
        let mut recovered_counts: Vec<usize> = Vec::new();
        for cut in cuts {
            std::fs::write(&cut_path, &bytes[..cut]).unwrap();
            match crate::recorder::rez::read_archive_bytes(&cut_path) {
                Ok((manifest, recordings)) => {
                    first_ok.get_or_insert(cut);
                    let rec = &manifest.recordings[0];
                    assert!(
                        !rec.complete,
                        "a truncated archive is never complete (cut {cut})"
                    );
                    let named: usize = rec.tables.iter().map(|t| t.files.len()).sum();
                    let recovered: usize = recordings[0].tables.iter().map(|(_, s)| s.len()).sum();
                    assert_eq!(
                        named, recovered,
                        "the resolved manifest names exactly the segments recovered (cut {cut})"
                    );
                    assert!(
                        !recordings[0].complete,
                        "a recovered recording is not complete (cut {cut})"
                    );
                    recovered_counts.push(recovered);
                }
                Err(e) => assert!(
                    first_ok.is_none(),
                    "cut {cut} failed after the archive first opened at {first_ok:?}: {e}"
                ),
            }
        }
        let first_ok = first_ok.expect("some prefix must open");
        assert!(
            first_ok <= 2048,
            "the initial manifest makes the archive readable almost immediately, got {first_ok}"
        );
        // The sweep must actually exercise partial recovery: an early prefix
        // recovers nothing, a late one recovers everything, and cuts in between
        // recover a prefix of the segments (each landing on the newest
        // checkpoint whose segments all survived).
        recovered_counts.sort_unstable();
        recovered_counts.dedup();
        assert_eq!(
            recovered_counts,
            // One rung per checkpoint: the initial (empty) manifest, then 1, 2
            // and 4 segments — the third batch sealed two samplers at once.
            vec![0, 1, 2, 4],
            "expected the full recovery ladder across the truncation sweep"
        );

        // The untruncated `.partial` still carries every sealed segment.
        let (manifest, _) = crate::recorder::rez::read_archive_bytes(&partial).unwrap();
        let named: usize = manifest.recordings[0]
            .tables
            .iter()
            .map(|t| t.files.len())
            .sum();
        assert_eq!(named, 4);
    }

    // Not a SIGKILL: `tar::Builder::drop` writes the footer and `Drop::join`
    // drains the queue, so this is graceful shutdown — the property that
    // matters for the recorder loop's early-return paths, which skip finalize.
    // The truncation sweep above covers the unclean-kill geometry.
    #[test]
    fn drop_without_finalize_leaves_recoverable_partial() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let mut handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle
            .seal(vec![job("cpu_usage", &[1_000, 2_000])])
            .unwrap();
        drop(handle);

        assert!(!out.exists(), "no output without a clean finalize");
        let (manifest, recordings) = crate::recorder::rez::read_archive_bytes(&partial).unwrap();
        let rec = &manifest.recordings[0];
        assert!(!rec.complete, "recovered recordings are not complete");
        assert_eq!(rec.tables.len(), 1);
        assert_eq!(rec.tables[0].files.len(), 1);
        assert_eq!(rec.tables[0].rows, 2);
        assert_eq!(recordings[0].tables[0].1.len(), 1);
        assert!(!recordings[0].complete);
    }

    #[test]
    fn abort_unlinks_partial() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);

        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        handle.abort();

        assert!(!partial.exists(), "abort unlinks the .partial");
        assert!(!out.exists(), "abort leaves no output");
    }

    // A leftover `.partial` may hold recoverable data from a previous crash, so
    // it is renamed aside, never clobbered.
    #[test]
    fn existing_partial_is_renamed_aside_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let partial = partial_of(&out);
        std::fs::write(&partial, b"old recoverable bytes").unwrap();

        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        let aside = dir.path().join("out.rez.partial.recovered-0");
        assert_eq!(std::fs::read(&aside).unwrap(), b"old recoverable bytes");
        assert!(crate::recorder::rez::is_rez_path(&partial).unwrap());
        handle.abort();

        // A second collision picks the next free suffix.
        std::fs::write(&partial, b"second crash").unwrap();
        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), b"old recoverable bytes");
        assert_eq!(
            std::fs::read(dir.path().join("out.rez.partial.recovered-1")).unwrap(),
            b"second crash"
        );
        handle.abort();
    }

    // A live concurrent writer is indistinguishable on disk from a leftover
    // `.partial`, and rename-aside runs first, so the second writer moves the
    // first one's file aside (never truncates it) and gets its own. O_EXCL only
    // closes the rename→create window; a real interlock would need a file lock.
    #[test]
    fn concurrent_create_moves_the_live_partial_aside() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let first = RezWriterHandle::create(&out, seed()).unwrap();
        let second = RezWriterHandle::create(&out, seed()).unwrap();
        assert!(dir.path().join("out.rez.partial.recovered-0").exists());
        second.abort();
        first.abort();
    }

    fn windowed_snap(i: u64) -> (metriken_exposition::Snapshot, u64) {
        let ts = 10_000 + i * 1_000;
        let end = 9_500 + i * 1_000;
        (
            snap(
                ts,
                vec![counter(
                    "0",
                    "cpu_usage",
                    i,
                    Some(Window::new(end - 500, end)),
                )],
            ),
            ts,
        )
    }

    #[test]
    fn thresholds_roll_segments_and_dedup_survives_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        let mut rec = StreamRecorder::with_policy(
            handle,
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows: 2,
                max_age: Duration::from_secs(3600),
            },
        );

        for i in 0..4u64 {
            let (s, ts) = windowed_snap(i);
            rec.ingest(&s, ts, 0);
            rec.maybe_seal().unwrap();
        }
        // Two full segments sealed; the open builder is empty.
        assert_eq!(rec.open_rows("cpu_usage"), 0);

        // The window of row 3 lives in an already-sealed segment: re-observing
        // it must still dedup, which only holds if `last_key` survived rotation.
        let (dup, dup_ts) = windowed_snap(3);
        rec.ingest(&dup, dup_ts + 1, 0);
        assert_eq!(rec.open_rows("cpu_usage"), 0, "dedup survived the seal");

        let (s, ts) = windowed_snap(4);
        rec.ingest(&s, ts, 0);
        rec.maybe_seal().unwrap();
        rec.finalize((ts, 0)).unwrap();

        let (manifest, _) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        let idx = &manifest.recordings[0].tables[0];
        assert_eq!(idx.rows, 5, "5 distinct observations, the 6th deduped");
        assert_eq!(idx.files.len(), 3, "ceil(5 / 2) segments");
        assert_eq!(
            idx.files,
            vec![
                "cpu_usage/0000.parquet",
                "cpu_usage/0001.parquet",
                "cpu_usage/0002.parquet"
            ]
        );
    }

    // The age bound exists for the kill-loss window: it must seal a builder that
    // no longer receives rows.
    #[test]
    fn age_threshold_seals_without_new_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        let mut rec = StreamRecorder::with_policy(
            handle,
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows: usize::MAX,
                max_age: Duration::from_millis(5),
            },
        );

        let (s, ts) = windowed_snap(0);
        rec.ingest(&s, ts, 0);
        std::thread::sleep(Duration::from_millis(20));
        rec.maybe_seal().unwrap();
        assert_eq!(rec.open_rows("cpu_usage"), 0, "age sealed the open builder");

        // A second row lands in a fresh segment, so two 1-row segments prove the
        // age seal happened (a missed seal would give one 2-row segment).
        let (s, ts) = windowed_snap(1);
        rec.ingest(&s, ts, 0);
        rec.finalize((ts, 0)).unwrap();

        let (manifest, _) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        let idx = &manifest.recordings[0].tables[0];
        assert_eq!(idx.files.len(), 2);
        assert_eq!(idx.rows, 2);
    }

    #[test]
    fn empty_builders_never_seal() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        let mut rec = StreamRecorder::with_policy(
            handle,
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows: 1,
                // Every builder is trivially past its age bound.
                max_age: Duration::ZERO,
            },
        );

        // Nothing ingested yet: no builders, nothing to seal.
        rec.maybe_seal().unwrap();

        let (s, ts) = windowed_snap(0);
        rec.ingest(&s, ts, 0);
        rec.maybe_seal().unwrap();
        // The builder is now empty but still present, and past its age bound.
        for _ in 0..3 {
            rec.maybe_seal().unwrap();
        }
        rec.finalize((ts, 0)).unwrap();

        let (manifest, _) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        let idx = &manifest.recordings[0].tables[0];
        assert_eq!(idx.files.len(), 1, "empty builders never seal");
        assert_eq!(idx.rows, 1);
    }

    #[test]
    fn no_data_recording_finalizes_as_an_empty_archive() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.rez");
        let handle = RezWriterHandle::create(&out, seed()).unwrap();
        let rec = StreamRecorder::new(handle);
        rec.finalize((1, 0)).unwrap();

        let (manifest, recordings) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        assert!(manifest.recordings[0].tables.is_empty());
        assert!(manifest.recordings[0].complete);
        assert!(recordings[0].tables.is_empty());
    }
}
