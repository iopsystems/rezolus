//! Copies of a dendro archive: a snapshot of a live one, a hindsight dump,
//! and the `recording` tools that rewrite one.
//!
//! dendro's `copy_sources_into` copies sealed segments as they are, the
//! clock offsets and caller rows, and encodes each stream's live WAL tail
//! into a final segment, all in one read snapshot of the source. The tail is
//! encoded with metriken-archive's `Encoder`, built from the source's stream
//! list, since a copy runs outside the writer that knows which streams are
//! long. The copy is therefore fully sealed.

use std::path::Path;

use dendro::archive::{Archive, ArchiveMut};
use dendro::rewrite::{copy_sources_into, CopySpec};

fn err(e: dendro::Error) -> String {
    e.to_string()
}

/// Every stream of every source in `src`.
pub(crate) fn streams(src: &Archive) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for source in src.read_sources().map_err(err)? {
        out.extend(src.all_streams(source.id).map_err(err)?);
    }
    Ok(out)
}

/// Copy `src` into a new archive at `dest`, which must not exist, as `spec`
/// selects. Returns how many sources were copied. The destination's write
/// handle is closed before this returns, so `dest` is one file with no
/// sidecars.
pub(crate) fn copy(src: &Archive, dest: &Path, spec: &CopySpec<'_>) -> Result<usize, String> {
    let names = streams(src)?;
    let encoder = metriken_archive::writer::Encoder::for_streams(names.iter().map(String::as_str));
    let mut dst = ArchiveMut::create(dest).map_err(err)?;
    let copied = dst
        .transaction(|tx| copy_sources_into(src, tx, spec, &encoder))
        .map_err(err)?;
    drop(dst);
    Ok(copied)
}

/// Test fixtures: dendro archives recorded through metriken-archive's writer,
/// so they carry what a real recording does (a long table and its occupant
/// stream beside a V2 sampler table).
#[cfg(test)]
pub(crate) mod fixtures {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, Snapshot, SnapshotV3};

    pub(crate) const ANCHOR: u64 = 1_700_000_000_000_000_000;
    pub(crate) const SECOND: u64 = 1_000_000_000;

    /// One tick: a slotted group `threads/tasks` with two threads, each a
    /// counter rising 10/s and 20/s.
    fn tick(i: u64) -> Snapshot {
        let ts = ANCHOR + i * SECOND;
        let member = |slot: u32, comm: &str| MetricDesc {
            name: format!("0x{slot}"),
            metadata: [
                ("metric", "task_ops"),
                ("id", &slot.to_string()),
                ("comm", comm),
                ("__uid__", &format!("u-{comm}")),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        };
        let schema = GroupSchema {
            counters: vec![member(0, "nginx"), member(1, "redis")],
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        Snapshot::V3(SnapshotV3 {
            systemtime: UNIX_EPOCH + Duration::from_nanos(ts),
            duration: Duration::ZERO,
            metadata: Default::default(),
            groups: vec![GroupSnapshot {
                name: "threads/tasks".to_string(),
                schema_hash: schema.hash(),
                schema: (i == 0).then(|| Arc::new(schema)),
                window: Some(metriken::Window::new(ts - SECOND / 2, ts)),
                counters: vec![Some(i * 10), Some(i * 20)],
                gauges: Vec::new(),
                histograms: Vec::new(),
            }],
        })
    }

    /// Record `ticks` ticks into a new archive at `path`, sealing every 4
    /// rows. Finalized when `finalize`; otherwise the writer is joined
    /// without finalizing, leaving the source incomplete with its last rows
    /// in the WAL, as a live or killed recording has them.
    pub(crate) fn recorded(path: &Path, ticks: u64, finalize: bool) {
        use metriken_archive::{ArchiveWriter, WriterConfig};
        let config = WriterConfig {
            seal: dendro::seal::SealPolicy {
                max_rows: 4,
                ..dendro::seal::SealPolicy::default()
            },
            ..WriterConfig::default()
        };
        let mut writer = ArchiveWriter::create(path, config).unwrap();
        let labels: BTreeMap<String, String> = [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect();
        let mut source = writer.add_source(labels, BTreeMap::new(), ANCHOR).unwrap();
        for i in 0..ticks {
            let staged = source.stage(&tick(i), ANCHOR + i * SECOND, 0).unwrap();
            writer.commit(vec![staged]).unwrap();
            source.maybe_seal().unwrap();
        }
        if finalize {
            source.finalize((ANCHOR + (ticks - 1) * SECOND, 0)).unwrap();
        } else {
            source.sync().unwrap();
            drop(source);
        }
        writer.join().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures;
    use super::*;

    /// A copy of an archive left with rows in its WAL is fully sealed, and
    /// holds every row: the tail becomes the copy's last segments.
    #[test]
    fn a_copy_seals_the_live_tail() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.dendro");
        fixtures::recorded(&live, 10, false);
        let src = Archive::open(&live).unwrap();
        let id = src.read_sources().unwrap()[0].id;
        let wal_rows: u64 = streams(&src)
            .unwrap()
            .iter()
            .map(|s| src.live_wal_span(id, s).unwrap().rows)
            .sum();
        assert!(wal_rows > 0, "the fixture leaves rows in the WAL");

        let dest = dir.path().join("copy.dendro");
        assert_eq!(copy(&src, &dest, &CopySpec::everything()).unwrap(), 1);
        for suffix in ["-wal", "-shm"] {
            assert!(
                !dir.path().join(format!("copy.dendro{suffix}")).exists(),
                "the copy is one file"
            );
        }
        let copied = Archive::open(&dest).unwrap();
        let cid = copied.read_sources().unwrap()[0].id;
        for stream in streams(&src).unwrap() {
            let (_, sealed) = src.segment_span(id, &stream).unwrap();
            let wal = src.live_wal_span(id, &stream).unwrap();
            let (_, got) = copied.segment_span(cid, &stream).unwrap();
            assert_eq!(
                copied.live_wal_span(cid, &stream).unwrap().rows,
                0,
                "{stream}"
            );
            assert_eq!(
                got.rows,
                sealed.rows + wal.rows,
                "{stream}: every row sealed"
            );
        }
        assert!(
            !copied.read_sources().unwrap()[0].complete,
            "an unfinished source stays so"
        );
    }
}
