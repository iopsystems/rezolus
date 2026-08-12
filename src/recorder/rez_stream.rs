//! Streaming `.rez` writer thread. See docs/journal/2026-08-11-rez-streaming-writer.md.
//!
//! `RezWriterHandle` owns a dedicated writer thread that encodes sealed
//! segments to parquet and appends them to the output tar. Sealing runs off the
//! scrape loop so a large parquet encode cannot skew the sampling cadence; the
//! channel is bounded, so a disk that cannot keep up backpressures the loop
//! instead of growing memory. The scrape-side half (`StreamRecorder`, which
//! decides when a segment is due) lands next.
//!
//! Contract: PANIC-FREE — every fallible op returns `Err`. The global panic
//! hook (`src/main.rs:57-62`) prints and calls `process::exit(101)` BEFORE
//! unwinding, so a panic here never reaches the send-error path, skips
//! finalize, and in wrapped mode orphans the child.

// The recorder loop wires these up in a follow-up change (D1); until then the
// tests at the bottom of this file are the only consumers.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;

use tracing::warn;

use super::rez::{
    append_tar_entry, write_table_parquet, RezManifest, RezRecording, RezTable, RezTableIndex,
    REZ_MANIFEST_NAME, REZ_SCHEMA_VERSION,
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
            // v1-shaped iff the table is a single segment; the streaming writer
            // always names segments `<sampler>/<seq>.parquet`, so this is only a
            // truthful alias, never a pointer at partial data.
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
        // Without this the dirent itself can be lost to a power cut, taking the
        // whole recovery artifact with it.
        sync_parent_dir(&partial)?;

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
        let thread_partial = partial.clone();
        let thread = std::thread::Builder::new()
            .name("rez-writer".to_string())
            .spawn(move || writer_thread(rx, builder, seed, thread_partial, output))
            .map_err(|e| format!("failed to spawn the .rez writer thread: {e}"))?;

        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            partial,
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
                seal_segments(&mut builder, &seed.dir, &mut tables, tails)?;
                clock_offsets.push(clock_offset);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::counter;
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

    /// A sealable job with `ts.len()` rows for `sampler`.
    fn job(sampler: &str, ts: &[u64]) -> SealJob {
        let mut b = TableBuilder::new(sampler.to_string());
        for (i, &t) in ts.iter().enumerate() {
            let c = counter("0", sampler, i as u64, Some(Window::new(t - 100, t)));
            b.push_row(t, 7, &[Entry::Counter(&c)]);
        }
        SealJob {
            sampler: sampler.to_string(),
            table: b.finish(),
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
        assert!(
            !rec.clock_offsets.is_empty(),
            "checkpoints append clock observations"
        );
        assert_eq!(rec.clock_offsets.last(), Some(&(5_000, 11)));

        // The bytes are readable by the truncation-tolerant reader.
        assert_eq!(recordings[0].tables.len(), 1);
        assert_eq!(recordings[0].tables[0].1.len(), 2, "two segment blobs");
        assert!(recordings[0].complete);
    }

    #[test]
    fn kill_before_finalize_leaves_recoverable_partial() {
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
}
