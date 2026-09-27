//! Convert a v3 `.rez` into a dendro archive.
//!
//! The two containers hold the same things under different names: a `.rez`
//! recording is a dendro source, a sampler table is a stream, and segments,
//! WAL rows, caller rows and clock offsets have the same shape in both.
//! Segment parquet BLOBs, WAL row payloads and caller row blobs are copied
//! byte-for-byte; only the catalog around them is rewritten.
//!
//! Two things change on the way:
//!
//! - **Timestamps become `i64`.** `rez` keeps them as `u64` and dendro as
//!   `i64`. A value above `i64::MAX` is refused, naming where it was, rather
//!   than cast: a cast would wrap it negative and put it before every other
//!   row. No `.rez` written by a real clock holds one, since `i64::MAX` ns is
//!   the year 2262.
//! - **Only the live WAL is copied.** Rows at or below a table's newest
//!   sealed segment are already in that segment; `rez` keeps them only until
//!   its next prune. Both containers compute the live tail with the same
//!   watermark, so a reader sees the same rows either way.
//!
//! Metadata and labels are copied as-is. The `version` key, which records
//! the version of the agent that was scraped, is also written under dendro's
//! [`PRODUCER_VERSION`](dendro::keys::PRODUCER_VERSION) key so a tool built on
//! dendro's conventions finds it. The `complete` flag is carried, so a
//! recording recovered from a crash still reads as recovered. Each source gets
//! a fresh dendro UUID: a `.rez` recording has no identity to carry over.
//!
//! No rezolus release reads the result yet; this exists to measure dendro's
//! read path on real recordings and as the migration path #1224 asks about.

use std::path::Path;

use dendro::archive::{ArchiveMut, CallerRow, SegmentMeta, SourceMeta, WalRow};

use crate::rez_sqlite::RezDb;

/// What one conversion wrote.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Converted {
    pub sources: usize,
    pub segments: usize,
    pub wal_rows: usize,
    pub caller_rows: usize,
    pub clock_offsets: usize,
}

/// Convert the v3 `.rez` at `src` into a new dendro archive at `dest`.
///
/// `dest` must not exist. The caller stages it and renames it into place, so
/// a failure part-way leaves no archive rather than a partial one.
pub fn convert_v3_to_dendro(src: &Path, dest: &Path) -> Result<Converted, String> {
    let db = RezDb::open(src)?;
    let mut out = ArchiveMut::create(dest)
        .map_err(|e| format!("failed to create {}: {e}", dest.display()))?;
    let mut done = Converted::default();

    for recording in db.read_recordings()? {
        let rec = recording.id;
        let mut metadata = recording.meta.metadata.clone();
        if let Some(version) = metadata.get("version").cloned() {
            metadata
                .entry(dendro::keys::PRODUCER_VERSION.to_string())
                .or_insert(version);
        }
        let meta = SourceMeta {
            labels: recording.meta.labels.clone(),
            metadata,
            clock_anchor_wall_ns: to_i64(recording.meta.clock_anchor_wall_ns, || {
                format!("recording {rec}'s clock anchor")
            })?,
        };
        let source = out.insert_source(&meta).map_err(|e| e.to_string())?;
        done.sources += 1;

        for sampler in db.all_samplers(rec)? {
            done.segments += copy_segments(&db, &mut out, rec, source, &sampler)?;
            let live = db.live_wal(rec, &sampler)?;
            let rows = live
                .into_iter()
                .map(|row| {
                    Ok(WalRow {
                        stream: row.sampler,
                        ts: to_i64(row.ts, || format!("a `{sampler}` WAL row"))?,
                        wall_offset: row.wall_offset,
                        row: row.row,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            done.wal_rows += rows.len();
            if !rows.is_empty() {
                out.insert_wal_rows(source, &rows)
                    .map_err(|e| e.to_string())?;
            }
        }

        for stream in db.caller_row_streams(rec)? {
            let rows = db
                .read_caller_rows(rec, &stream, 0, u64::MAX)?
                .into_iter()
                .map(|(ts, blob)| {
                    Ok(CallerRow {
                        ts: to_i64(ts, || format!("a `{stream}` caller row"))?,
                        blob,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            done.caller_rows += rows.len();
            out.insert_caller_rows(source, &stream, &rows)
                .map_err(|e| e.to_string())?;
        }

        let offsets = db
            .read_clock_offsets(rec)?
            .into_iter()
            .map(|(ts, offset)| Ok((to_i64(ts, || "a clock offset".to_string())?, offset)))
            .collect::<Result<Vec<_>, String>>()?;
        done.clock_offsets += offsets.len();
        out.transaction(|tx| {
            for &(ts, offset) in &offsets {
                tx.insert_clock_offset(source, ts, offset)?;
            }
            if recording.complete {
                tx.mark_complete(source)?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(done)
}

/// Copy one sampler's segments, one at a time, so a large table is never
/// held in memory whole.
fn copy_segments(
    db: &RezDb,
    out: &mut ArchiveMut,
    rec: i64,
    source: i64,
    sampler: &str,
) -> Result<usize, String> {
    let metas = db.read_segment_meta(rec, sampler)?;
    for (seq, meta) in &metas {
        let bytes = db
            .read_segment_bytes(rec, sampler, *seq)?
            .ok_or_else(|| format!("segment {sampler}#{seq} disappeared during the conversion"))?;
        let meta = SegmentMeta {
            rows: meta.rows,
            first_ts: to_i64(meta.first_ts, || {
                format!("segment {sampler}#{seq}'s first_ts")
            })?,
            last_ts: to_i64(meta.last_ts, || {
                format!("segment {sampler}#{seq}'s last_ts")
            })?,
        };
        out.insert_segment(source, sampler, *seq, &meta, &bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(metas.len())
}

/// A `rez` timestamp as dendro's `i64`, refusing one that does not fit.
fn to_i64(value: u64, what: impl FnOnce() -> String) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| {
        format!(
            "{} is {value}, above i64::MAX; dendro stores timestamps as i64 and a \
             cast would wrap it negative",
            what()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rez_sqlite::{RecordingMeta, SegmentMeta as RezSegmentMeta, WalRow as RezWalRow};
    use dendro::archive::{Archive, Depth};
    use std::collections::BTreeMap;

    const TASK: &str = "cpu_usage/cpu_usage_task";

    fn wal(sampler: &str, ts: u64) -> RezWalRow {
        RezWalRow {
            sampler: sampler.to_string(),
            ts,
            wall_offset: 7,
            row: vec![ts as u8, 0xAB],
        }
    }

    /// Two recordings. The first is finalized and has two sealed segments on
    /// one table, a WAL row those segments already cover, two live WAL rows,
    /// caller rows on that table (two at one timestamp) and on a name no table
    /// uses, and clock offsets. The second is unfinished and has only WAL.
    fn fixture(path: &Path) {
        let mut db = RezDb::create(path).unwrap();
        let meta = |labels: &[(&str, &str)], metadata: &[(&str, &str)]| RecordingMeta {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            metadata: metadata
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            clock_anchor_wall_ns: 1_700_000_000_000_000_000,
        };
        let a = db
            .insert_recording(&meta(
                &[("source", "rezolus"), ("host", "a")],
                &[("version", "5.22.1"), ("sampling_interval_ms", "1000")],
            ))
            .unwrap();
        let b = db
            .insert_recording(&meta(&[("source", "rezolus"), ("host", "b")], &[]))
            .unwrap();
        db.transaction(|tx| {
            for (seq, first, last) in [(0u64, 1u64, 3u64), (1, 4, 6)] {
                tx.insert_segment(
                    a,
                    TASK,
                    seq,
                    &RezSegmentMeta {
                        rows: 3,
                        first_ts: first,
                        last_ts: last,
                    },
                    format!("segment {seq}").as_bytes(),
                )?;
            }
            tx.insert_wal_rows(a, &[wal(TASK, 5), wal(TASK, 7), wal(TASK, 8)])?;
            tx.insert_caller_rows(
                a,
                TASK,
                &[
                    (1, b"full".to_vec()),
                    (1, b"delta a".to_vec()),
                    (4, b"delta b".to_vec()),
                ],
            )?;
            tx.insert_caller_rows(a, "notes", &[(2, b"note".to_vec())])?;
            tx.insert_clock_offset(a, 3, 10)?;
            tx.insert_clock_offset(a, 6, 11)?;
            tx.mark_complete(a)?;
            tx.insert_wal_rows(b, &[wal("blockio", 1), wal("blockio", 2)])
        })
        .unwrap();
    }

    /// Every table of every recording in `src` against the source it became
    /// in `dest`, in the same order: catalog, segment bytes, live WAL, caller
    /// rows and clock offsets. Also runs dendro's own `verify`.
    fn assert_same(src: &Path, dest: &Path) {
        let rez = RezDb::open(src).unwrap();
        let out = Archive::open(dest).unwrap();
        let recordings = rez.read_recordings().unwrap();
        let sources = out.read_sources().unwrap();
        assert_eq!(recordings.len(), sources.len());
        for (rec, source) in recordings.iter().zip(&sources) {
            let (r, s) = (rec.id, source.id);
            assert_eq!(rec.meta.labels, source.meta.labels);
            for (k, v) in &rec.meta.metadata {
                assert_eq!(source.meta.metadata.get(k), Some(v), "metadata `{k}`");
            }
            assert_eq!(
                rec.meta.clock_anchor_wall_ns as i64,
                source.meta.clock_anchor_wall_ns
            );
            assert_eq!(rec.complete, source.complete);
            assert!(source.uuid.is_some(), "dendro mints one");

            let samplers = rez.all_samplers(r).unwrap();
            assert_eq!(samplers, out.all_streams(s).unwrap());
            for sampler in &samplers {
                let metas = rez.read_segment_meta(r, sampler).unwrap();
                let segments = out.read_segments(s, sampler).unwrap();
                assert_eq!(metas.len(), segments.len(), "{sampler} segments");
                for ((seq, meta), seg) in metas.iter().zip(&segments) {
                    assert_eq!(*seq, seg.seq);
                    assert_eq!(
                        (meta.rows, meta.first_ts as i64, meta.last_ts as i64),
                        (seg.meta.rows, seg.meta.first_ts, seg.meta.last_ts),
                        "{sampler}#{seq}"
                    );
                    let bytes = rez.read_segment_bytes(r, sampler, *seq).unwrap().unwrap();
                    assert!(bytes == seg.bytes, "{sampler}#{seq} bytes differ");
                }
                let live: Vec<_> = rez
                    .live_wal(r, sampler)
                    .unwrap()
                    .into_iter()
                    .map(|w| (w.ts as i64, w.wall_offset, w.row))
                    .collect();
                let got: Vec<_> = out
                    .live_wal(s, sampler)
                    .unwrap()
                    .into_iter()
                    .map(|w| (w.ts, w.wall_offset, w.row))
                    .collect();
                assert_eq!(live, got, "{sampler} live WAL");
            }

            let names = rez.caller_row_streams(r).unwrap();
            assert_eq!(names, out.caller_row_streams(s).unwrap());
            for name in &names {
                let want: Vec<_> = rez
                    .read_caller_rows(r, name, 0, u64::MAX)
                    .unwrap()
                    .into_iter()
                    .map(|(ts, blob)| (ts as i64, blob))
                    .collect();
                let got: Vec<_> = out
                    .read_caller_rows(s, name, i64::MIN, i64::MAX)
                    .unwrap()
                    .into_iter()
                    .map(|row| (row.ts, row.blob))
                    .collect();
                assert_eq!(want, got, "`{name}` caller rows");
            }

            let offsets: Vec<_> = rez
                .read_clock_offsets(r)
                .unwrap()
                .into_iter()
                .map(|(ts, off)| (ts as i64, off))
                .collect();
            assert_eq!(offsets, out.read_clock_offsets(s).unwrap());
        }
        let report = out.verify(Depth::Quick).unwrap();
        assert!(report.is_sound(), "{:?}", report.problems);
    }

    #[test]
    fn a_converted_archive_holds_what_the_rez_held() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.rez");
        let dest = dir.path().join("out.dendro");
        fixture(&src);
        let done = convert_v3_to_dendro(&src, &dest).unwrap();
        assert_eq!(
            done,
            Converted {
                sources: 2,
                segments: 2,
                wal_rows: 4,
                caller_rows: 4,
                clock_offsets: 2,
            },
            "the WAL row at 5 is under segment 1 and is not copied"
        );
        assert_same(&src, &dest);

        let out = Archive::open(&dest).unwrap();
        let a = &out.read_sources().unwrap()[0];
        assert_eq!(
            a.meta.metadata.get(dendro::keys::PRODUCER_VERSION),
            Some(&"5.22.1".to_string()),
            "the agent's version under dendro's key as well as rezolus's"
        );
        assert_eq!(
            out.read_wal(a.id, TASK).unwrap().len(),
            2,
            "only live rows were copied"
        );
    }

    #[test]
    fn rez_refuses_the_output_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.rez");
        let dest = dir.path().join("out.dendro");
        fixture(&src);
        convert_v3_to_dendro(&src, &dest).unwrap();
        let err = RezDb::open(&dest)
            .err()
            .expect("a dendro archive is not a .rez");
        assert!(err.contains("is a dendro archive"), "{err}");
        let err = RezDb::open_bytes(std::fs::read(&dest).unwrap())
            .err()
            .expect("nor as bytes");
        assert!(err.contains("is a dendro archive"), "{err}");
    }

    #[test]
    fn a_timestamp_above_i64_max_is_refused_not_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.rez");
        let db = RezDb::create(&src).unwrap();
        let rec = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 0,
            })
            .unwrap();
        // A segment rather than a WAL row: stored as `i64`, `u64::MAX` reads
        // back as -1, which is at or below any watermark, so a WAL row would
        // never be live and never reach the conversion.
        db.insert_segment(
            rec,
            "cpu_usage",
            0,
            &RezSegmentMeta {
                rows: 1,
                first_ts: 1,
                last_ts: u64::MAX,
            },
            b"x",
        )
        .unwrap();
        drop(db);
        let err = convert_v3_to_dendro(&src, &dir.path().join("out.dendro")).unwrap_err();
        assert!(
            err.contains("segment cpu_usage#0's last_ts") && err.contains("above i64::MAX"),
            "{err}"
        );
    }

    /// Converts a real archive and compares it table by table:
    ///
    /// ```text
    /// REZ_TO_DENDRO=/path/to/archive.rez \
    ///   cargo test -p rez --features test-support --release \
    ///   converts_a_real_archive -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs REZ_TO_DENDRO=<path to a v3 .rez>"]
    fn converts_a_real_archive() {
        let src =
            std::path::PathBuf::from(std::env::var_os("REZ_TO_DENDRO").expect("set REZ_TO_DENDRO"));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.dendro");
        let start = std::time::Instant::now();
        let done = convert_v3_to_dendro(&src, &dest).unwrap();
        let took = start.elapsed();
        println!(
            "{done:?} in {took:.2?}: {} -> {} bytes",
            std::fs::metadata(&src).unwrap().len(),
            std::fs::metadata(&dest).unwrap().len()
        );
        assert_same(&src, &dest);
    }
}
