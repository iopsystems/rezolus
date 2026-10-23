//! Archives for tests, written as dendro archives through metriken-storage's
//! `ArchiveWriter`, and the committed `.rez` archives for tests about reading
//! that container.
//!
//! Nothing here writes a `.rez`, and metriken-storage 0.5 has no `.rez`
//! writer. A test whose subject is reading a `.rez` (a tar, a v3 read from
//! bytes, a live v3 tail) reads one of the archives in `tests/fixtures/rez/`
//! ([`committed_rez`]), changed with SQL where it needs another catalog state
//! ([`edited_rez`]); every other test writes a `.dendro` with
//! [`write_dendro`] or [`one_run`].
//!
//! [`one_run`] builds the archive the viewers' tests of the combined view
//! share: the server viewer's (in the binary crate) and the WASM viewer's
//! (`crates/viewer`), which build the same archive so they can be compared.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};
use metriken_storage::{ArchiveWriter, WriterConfig};

use crate::window::Window;

/// The first row's time, in ns since the epoch.
pub const ANCHOR: u64 = 1_700_000_000_000_000_000;

/// How many one-second rows each recording of [`one_run`] holds.
pub const TICKS: u64 = 10;

/// A label or metadata map from pairs.
pub fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// One source of [`write_dendro`]: its labels, its metadata, and its rows as
/// `(snapshot, timestamp in ns)`, in time order.
pub struct Source {
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub rows: Vec<(Snapshot, u64)>,
}

impl Source {
    /// A source labelled `source=rezolus` with no metadata.
    pub fn rezolus(rows: Vec<(Snapshot, u64)>) -> Self {
        Self {
            labels: map(&[("source", "rezolus")]),
            metadata: BTreeMap::new(),
            rows,
        }
    }
}

/// How [`write_dendro`] leaves each source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Every stream sealed and the source marked complete, as `record` ends.
    Finalized,
    /// The rows committed and nothing sealed beyond what `seal_rows` sealed:
    /// the newest rows stay in the archive's WAL and the source is
    /// incomplete, as a recording that is still running (or was killed) has
    /// them.
    Live,
}

/// Write `sources` into a new dendro archive at `path`, one source each.
///
/// Row `i` of every source is committed in one tick, so sources of unequal
/// length are fine. With `seal_rows`, each stream is sealed every that many
/// rows (`SealPolicy::max_rows`; dendro staggers a stream's first segment by
/// its stream and source, so the first seal can come sooner); without, only
/// finalizing seals, so a finalized stream is one segment. A source's clock
/// anchor is its first row's time. No source carries a `producer_epoch`.
pub fn write_dendro(path: &Path, sources: &[Source], seal_rows: Option<usize>, finish: Finish) {
    let config = WriterConfig {
        seal: dendro::seal::SealPolicy {
            max_bytes: usize::MAX,
            max_rows: seal_rows.unwrap_or(usize::MAX),
            max_age: Duration::from_secs(24 * 3600),
            ..dendro::seal::SealPolicy::default()
        },
        ..WriterConfig::default()
    };
    let mut writer = ArchiveWriter::create(path, config).unwrap();
    let mut recorders: Vec<_> = sources
        .iter()
        .map(|s| {
            let anchor = s.rows.first().map_or(ANCHOR, |(_, ts)| *ts);
            writer
                .add_source(s.labels.clone(), s.metadata.clone(), anchor)
                .unwrap()
        })
        .collect();
    let ticks = sources.iter().map(|s| s.rows.len()).max().unwrap_or(0);
    for i in 0..ticks {
        let staged = recorders
            .iter_mut()
            .zip(sources)
            .filter_map(|(r, s)| {
                let (snapshot, ts) = s.rows.get(i)?;
                Some(r.stage(snapshot, *ts, 0).unwrap())
            })
            .collect();
        writer.commit(staged).unwrap();
        if seal_rows.is_some() {
            for r in &mut recorders {
                r.maybe_seal().unwrap();
            }
        }
    }
    for (mut r, s) in recorders.into_iter().zip(sources) {
        match finish {
            Finish::Finalized => {
                let last = s.rows.last().map_or(ANCHOR, |(_, ts)| *ts);
                r.finalize((last, 0)).unwrap();
            }
            Finish::Live => r.sync().unwrap(),
        }
    }
    writer.join().unwrap();
}

/// `stream -> segment count` of each source of the dendro archive at `path`,
/// in catalog order.
pub fn segment_counts(path: &Path) -> Vec<BTreeMap<String, usize>> {
    let archive = dendro::archive::Archive::open(path).unwrap();
    archive
        .read_sources()
        .unwrap()
        .iter()
        .map(|source| {
            archive
                .all_streams(source.id)
                .unwrap()
                .into_iter()
                .map(|stream| {
                    let n = archive.read_segment_meta(source.id, &stream).unwrap().len();
                    (stream, n)
                })
                .collect()
        })
        .collect()
}

/// A counter named `name` in `sampler`, its `metric` its name.
pub fn counter(name: &str, sampler: &str, value: u64, window: Option<Window>) -> Counter {
    Counter::new(
        name.to_string(),
        value,
        meta(&[("metric", name), ("sampler", sampler)]),
    )
    .with_window(window)
}

/// A gauge named `name` in `sampler`, its `metric` its name.
pub fn gauge(name: &str, sampler: &str, value: i64, window: Option<Window>) -> Gauge {
    Gauge::new(
        name.to_string(),
        value,
        meta(&[("metric", name), ("sampler", sampler)]),
    )
    .with_window(window)
}

/// A V2 snapshot at `ts` (ns since the epoch).
pub fn snap(ts: u64, counters: Vec<Counter>, gauges: Vec<Gauge>) -> Snapshot {
    Snapshot::V2(SnapshotV2 {
        systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
        duration: Duration::ZERO,
        metadata: HashMap::new(),
        counters,
        gauges,
        histograms: Vec::new(),
    })
}

/// A copy, in `dir`, of the committed `.rez` archive `name` (opening a v3
/// archive read-write may create SQLite sidecars beside it, which must not
/// land in the source tree).
///
/// The archives are metriken-storage's (`metriken-storage/tests/fixtures/rez`
/// at iopsystems/metriken d8efc35), written once by the last `.rez` writers
/// metriken had, at 0c39b2d (after metriken-storage 0.4.0, their output kept
/// at 0.4.0's shape). Every recording but `v3-multi.rez`'s holds 12 ticks, 1 s
/// apart from 1700000000 s, labelled `source=rezolus,host=web-01`: `ops` (a
/// windowed counter, +10 a tick, sampler `a`), `mem_free` (a gauge,
/// 1000 − t, sampler `b`) and `lat` (a windowed histogram at grouping power
/// 2, max value power 8, one more observation of 2 each tick, sampler `a`).
///
/// - `v2.rez`: a v2 tar written whole, one segment per table, complete.
/// - `v3.rez`: v3, four rows a segment, finalized.
/// - `v3-live.rez`: v3 from V3 snapshots (group tables `a/g`, `b/g`), five
///   rows a segment, not finalized: sealed segments and live WAL rows.
/// - `v3-multi.rez`: v3, two recordings `host=web-01` and `host=web-02`,
///   three ticks of `mem_free` each, `(i + 1) * 1000 + t` for recording `i`.
pub fn committed_rez(name: &str, dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rez")
        .join(name);
    let dest = dir.join(name);
    std::fs::copy(&src, &dest).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    dest
}

fn meta(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// One recording of [`one_run`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Recording<'a> {
    pub labels: &'a [(&'a str, &'a str)],
    pub metadata: &'a [(&'a str, &'a str)],
    /// How many CPUs the recording's `cpu_usage` has (`id` 0 to n - 1),
    /// with `cpu_cores` the count. None when 0.
    pub cpus: u64,
    /// Each CPU's busy nanoseconds per second, so `sum(irate(cpu_usage[5m]))
    /// / cpu_cores / 1e9` is `busy_ns / 1e9`.
    pub busy_ns: u64,
    /// A counter of this name rising `rate` per second, for a service.
    pub counter: Option<(&'a str, u64)>,
}

/// A finalized dendro archive at `path` holding `recordings`, a source each,
/// each written over the same [`TICKS`] seconds from [`ANCHOR`]: one run.
pub fn one_run(path: &Path, recordings: &[Recording<'_>]) {
    let sources: Vec<Source> = recordings
        .iter()
        .map(|r| Source {
            labels: map(r.labels),
            metadata: map(r.metadata),
            rows: (0..TICKS).map(|t| one_run_row(r, t)).collect(),
        })
        .collect();
    write_dendro(path, &sources, None, Finish::Finalized);
}

fn one_run_row(r: &Recording<'_>, t: u64) -> (Snapshot, u64) {
    let ts = ANCHOR + t * 1_000_000_000;
    let window = Some(Window::new(ts - 1_000_000, ts));
    let mut counters: Vec<Counter> = (0..r.cpus)
        .map(|id| {
            Counter::new(
                format!("cpu_usage/user/{id}"),
                r.busy_ns * t,
                meta(&[
                    ("metric", "cpu_usage"),
                    ("sampler", "cpu_usage"),
                    ("id", &id.to_string()),
                    ("state", "user"),
                ]),
            )
            .with_window(window)
        })
        .collect();
    let mut gauges: Vec<Gauge> = Vec::new();
    if r.cpus > 0 {
        gauges.push(
            Gauge::new(
                "cpu_cores".to_string(),
                r.cpus as i64,
                meta(&[("metric", "cpu_cores"), ("sampler", "cpu_usage")]),
            )
            .with_window(window),
        );
    }
    if let Some((name, rate)) = r.counter {
        counters.push(
            Counter::new(
                name.to_string(),
                rate * t,
                meta(&[("metric", name), ("sampler", "service")]),
            )
            .with_window(window),
        );
    }
    (snap(ts, counters, gauges), ts)
}

/// The sources of [`multi_recording`]: one per `(source, host)` pair, each
/// labelled with those two, `source` also in its metadata, and three rows of
/// a `cpu_usage` counter `cpu_cycles` whose values are unique to the
/// recording, `(i + 1) * 1000 + t` for recording `i` at tick `t`.
///
/// The values differ per recording on purpose. A consumer that picks the
/// wrong arm still returns *some* series, so a test asserting only that data
/// came back would pass either way; distinguishable values make the wrong arm
/// fail.
pub fn multi_recording_sources(recordings: &[(&str, &str)]) -> Vec<Source> {
    recordings
        .iter()
        .enumerate()
        .map(|(i, (source, host))| Source {
            labels: map(&[("source", source), ("host", host)]),
            metadata: map(&[("source", source)]),
            rows: (0..3u64)
                .map(|t| {
                    let ts = ANCHOR + 1_000_000_000 * (t + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    let v = (i as u64 + 1) * 1_000 + t;
                    (
                        snap(ts, vec![counter("cpu_cycles", "cpu_usage", v, w)], vec![]),
                        ts,
                    )
                })
                .collect(),
        })
        .collect()
}

/// A finalized dendro archive at `path` of [`multi_recording_sources`].
pub fn multi_recording(path: &Path, recordings: &[(&str, &str)]) {
    write_dendro(
        path,
        &multi_recording_sources(recordings),
        None,
        Finish::Finalized,
    );
}

/// Add `extra` to the metadata of recording `index` (catalog order) of the
/// v3 `.rez` at `path`, as `recording annotate` and Save as Report write a
/// `.rez`'s metadata (until they write `.dendro` only). For a test that reads
/// keys a `.rez` can carry, on a copy of a committed fixture
/// ([`committed_rez`]).
pub fn add_rez_metadata(path: &Path, index: usize, extra: &[(&str, &str)]) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let (id, metadata): (i64, String) = conn
        .query_row(
            "SELECT id, metadata FROM recordings ORDER BY id LIMIT 1 OFFSET ?1",
            [index as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut metadata: BTreeMap<String, String> = serde_json::from_str(&metadata).unwrap();
    metadata.extend(map(extra));
    conn.execute(
        "UPDATE recordings SET metadata = ?1 WHERE id = ?2",
        rusqlite::params![serde_json::to_string(&metadata).unwrap(), id],
    )
    .unwrap();
}

/// A V2 snapshot at `ts` holding only `counters`.
pub fn snap_counters(ts: u64, counters: Vec<Counter>) -> Snapshot {
    snap(ts, counters, Vec::new())
}

/// Append to the v3 `.rez` at `path` one more live tick at `ts`: a copy of
/// each table's newest WAL row, as a 5.x recorder appended one. For a test
/// that follows a `.rez` while it grows, on a copy of the committed
/// `v3-live.rez` ([`committed_rez`]), whose newest rows carry no schema and
/// so decode against the schema an earlier row stated.
pub fn append_rez_wal_tick(path: &Path, ts: u64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let n = conn
        .execute(
            "INSERT INTO wal (recording_id, sampler, ts, wall_offset, row) \
             SELECT w.recording_id, w.sampler, ?1, w.wall_offset, w.row FROM wal w \
             WHERE w.ts = (SELECT MAX(ts) FROM wal v \
                           WHERE v.recording_id = w.recording_id AND v.sampler = w.sampler)",
            [ts as i64],
        )
        .unwrap();
    assert!(n > 0, "{} has no WAL rows to extend", path.display());
}

/// A copy at `path` of the committed single-recording `v3.rez`
/// ([`committed_rez`]) whose recording's labels are `labels` and whose
/// metadata also holds `metadata`: a `.rez` input with the identity a test
/// needs, as a 5.x recorder would have written it.
pub fn rez_with_identity(path: &Path, labels: &[(&str, &str)], metadata: &[(&str, &str)]) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rez/v3.rez");
    std::fs::copy(&src, path).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    let conn = rusqlite::Connection::open(path).unwrap();
    let n = conn
        .execute(
            "UPDATE recordings SET labels = ?1",
            [serde_json::to_string(&map(labels)).unwrap()],
        )
        .unwrap();
    assert_eq!(n, 1, "v3.rez holds one recording");
    drop(conn);
    add_rez_metadata(path, 0, metadata);
}

/// A copy at `dest` of the committed `.rez` `name` ([`committed_rez`]) with
/// `sql` applied to it: the catalog states a test of a `.rez` reader needs
/// (a table with no sealed segment, a straddling WAL row, chosen clock
/// offsets, segment bytes that are not parquet) carved from an archive the
/// last `.rez` writer produced.
pub fn edited_rez(name: &str, dest: &Path, sql: &str) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rez")
        .join(name);
    std::fs::copy(&src, dest).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    rusqlite::Connection::open(dest)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// A v2 tar `.rez` at `dest` holding one recording per entry of `labels`,
/// each a copy of the committed `v2.rez`'s one recording (its tables `a` and
/// `b`, 12 ticks) under those labels and its own directory `rec-<i>`: the
/// multi-recording tar a 5.x `recording combine` or `record --endpoint a
/// --endpoint b -o out.rez` wrote. The recordings hold the same rows.
pub fn tar_rez(dest: &Path, labels: &[&[(&str, &str)]]) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rez/v2.rez");
    let mut archive = tar::Archive::new(std::fs::File::open(&src).unwrap());
    let mut manifest: Option<serde_json::Value> = None;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        if path == "manifest.json" {
            manifest = Some(serde_json::from_slice(&bytes).unwrap());
        } else {
            let (_, file) = path.split_once('/').expect("<dir>/<file>");
            files.push((file.to_string(), bytes));
        }
    }
    let mut manifest = manifest.expect("v2.rez has a manifest");
    let template = manifest["recordings"][0].clone();
    let recordings: Vec<serde_json::Value> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut r = template.clone();
            r["dir"] = format!("rec-{i}").into();
            r["labels"] = serde_json::to_value(map(l)).unwrap();
            r
        })
        .collect();
    manifest["recordings"] = recordings.into();

    fn append(out: &mut tar::Builder<std::fs::File>, name: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        out.append_data(&mut header, name, bytes).unwrap();
    }
    let mut out = tar::Builder::new(std::fs::File::create(dest).unwrap());
    append(
        &mut out,
        "manifest.json",
        &serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    for i in 0..labels.len() {
        for (file, bytes) in &files {
            append(&mut out, &format!("rec-{i}/{file}"), bytes);
        }
    }
    out.finish().unwrap();
}
