//! Path-shaped adapter over the shared `report-save` crate. The server's
//! `save_with_selection` handler has paths on disk; this reads them into
//! bytes and hands them to `report_save::build_report`, which the WASM
//! static-site viewer shares. A report is always a `.dendro`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub use report_save::ReportPayload;

/// Build a dendro report of the recordings at `paths`: one archive, or the
/// two of a two-file comparison (baseline first). See
/// `report_save::build_report` for the semantics (trim when `keep_metrics`
/// is `Some`, keep what `range` holds, embed the selection and events, stamp
/// the report marker on the recording labelled `anchor`).
pub fn build_report(
    paths: &[&Path],
    anchor: Option<&BTreeMap<String, String>>,
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<::report_save::TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use ::report_save::ReportSource;
    let read =
        |p: &Path| std::fs::read(p).map_err(|e| format!("failed to read {}: {e}", p.display()));
    let build = |source| {
        ::report_save::build_report(
            source,
            anchor,
            keep_metrics,
            range,
            selection_json,
            events_json,
        )
    };
    match paths {
        [one] => build(ReportSource::Dendro(&read(one)?)),
        [baseline, experiment] => build(ReportSource::Compare {
            baseline: &read(baseline)?,
            experiment: &read(experiment)?,
        }),
        _ => Err("nothing to save".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parquet_metadata::{KEY_EVENTS, KEY_REPORT, KEY_SELECTION, REPORT_VALUE_TRIMMED};

    /// The committed legacy fixture `name` upgraded to a `.dendro` in
    /// `dir`, as a user upgrades it before opening it.
    fn upgraded(name: &str, dir: &Path) -> std::path::PathBuf {
        let src = ::rez::fixtures::committed_rez(name, dir);
        let out = dir.join(format!("{name}.dendro"));
        crate::parquet_tools::legacy::convert(&src, &out).unwrap();
        out
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A dendro report's streams and its first source's metadata.
    fn dendro_report(bytes: &[u8]) -> (Vec<String>, BTreeMap<String, String>) {
        let db = dendro::archive::Archive::open_bytes(bytes.to_vec()).unwrap();
        let sources = db.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        let mut streams = db.all_streams(sources[0].id).unwrap();
        streams.sort();
        (streams, sources[0].meta.metadata.clone())
    }

    /// An archive saves a trimmed report: the selection and the marker on
    /// its recording, the table holding none of the kept metrics dropped.
    /// The source is not changed.
    #[test]
    fn an_upgraded_rez_saves_a_trimmed_dendro_report() {
        let dir = tempfile::tempdir().unwrap();
        let src = upgraded("v3.rez", dir.path());
        let before = std::fs::read(&src).unwrap();
        // `ops` is table `a`'s; `mem_free` is table `b`'s.
        let bytes = build_report(
            &[&src],
            None,
            Some(&set(&["ops"])),
            None,
            r#"{"entries":[]}"#,
            None,
        )
        .unwrap();
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(streams, ["a"]);
        assert_eq!(
            md.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[]}"#)
        );
        assert_eq!(
            md.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );
        assert_eq!(std::fs::read(&src).unwrap(), before);
    }

    /// An untrimmed report keeps every table and the selection, and no
    /// marker; a legacy file is refused.
    #[test]
    fn an_untrimmed_report_keeps_every_table_and_a_legacy_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = ::rez::fixtures::committed_rez("v2.rez", dir.path());
        let err = build_report(&[&legacy], None, None, None, "{}", None).unwrap_err();
        assert!(err.contains(".rez archive"), "{err}");
        let src = upgraded("v2.rez", dir.path());
        let bytes = build_report(&[&src], None, None, None, "{}", None).unwrap();
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(streams, ["a", "b"]);
        assert!(md.contains_key(KEY_SELECTION));
        assert!(!md.contains_key(KEY_REPORT));
    }

    /// A save whose payload has no events clears any events the source
    /// carried.
    #[test]
    fn a_save_with_no_events_clears_a_stale_events_key() {
        let dir = tempfile::tempdir().unwrap();
        let edited = dir.path().join("src.rez");
        ::rez::fixtures::edited_rez(
            "v3.rez",
            &edited,
            r#"UPDATE recordings SET metadata = '{"events":"{\"events\":[{\"timestamp\":1,\"description\":\"x\"}]}"}';"#,
        );
        let src = dir.path().join("src.dendro");
        crate::parquet_tools::legacy::convert(&edited, &src).unwrap();
        let bytes = build_report(&[&src], None, None, None, "{}", None).unwrap();
        let (_, md) = dendro_report(&bytes);
        assert!(
            !md.contains_key(KEY_EVENTS),
            "a save with no events drops the stale key"
        );
    }

    /// `sum by (comm) (rate(task_ops[3s]))` over the archive at `path`.
    fn task_rates(path: &Path) -> Vec<(String, Vec<(f64, f64)>)> {
        use metriken_query::MetricsSource;
        let r = crate::rez_reader::RezReader::open_recordings(
            path,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
        let (start, end) = r.time_range().unwrap();
        let metriken_query::QueryResult::Matrix { result } = r
            .query_range("sum by (comm) (rate(task_ops[3s]))", start, end + 1.0, 1.0)
            .unwrap()
        else {
            panic!("a matrix");
        };
        let mut out: Vec<_> = result
            .into_iter()
            .map(|s| (s.metric.get("comm").cloned().unwrap_or_default(), s.values))
            .collect();
        out.sort_by(|x, y| x.0.cmp(&y.0));
        out
    }

    /// A trimmed report of a dendro source: the long table stays long with
    /// its occupant stream, the untouched table goes, the markers sit on the
    /// source, and the kept metric answers the same. The source is
    /// unfinalized, so its last rows come from the WAL tail.
    #[test]
    fn a_dendro_source_saves_a_trimmed_dendro_report() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, false);

        let bytes = build_report(
            &[&src],
            None,
            Some(&set(&["task_ops"])),
            None,
            r#"{"entries":[]}"#,
            None,
        )
        .unwrap();
        assert!(metriken_storage::DendroCatalog::is_archive_bytes(&bytes));
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(streams, ["threads/tasks", "threads/tasks/occupants"]);
        assert_eq!(
            md.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[]}"#)
        );
        assert_eq!(
            md.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );

        // The browser viewer opens an upload from its bytes.
        let from_bytes = crate::rez_reader::RezReader::open_recordings_from_bytes(
            bytes.clone(),
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(from_bytes.len(), 1);

        let out = dir.path().join("report.dendro");
        std::fs::write(&out, &bytes).unwrap();
        let (a, b) = (task_rates(&src), task_rates(&out));
        assert_eq!(a.len(), 2, "two threads, by their labels: {a:?}");
        assert_eq!(a, b, "the kept metric answers the same");
    }

    /// A trim that keeps no metric of a long table drops its occupant stream
    /// too, rather than leaving an occupant stream with no table.
    #[test]
    fn a_dendro_report_drops_the_occupant_stream_of_a_dropped_table() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);

        let bytes =
            build_report(&[&src], None, Some(&set(&["mem_free"])), None, "{}", None).unwrap();
        let (streams, _) = dendro_report(&bytes);
        assert_eq!(streams, ["memory/meminfo"]);
    }

    /// An untrimmed dendro report copies every stream and carries no report
    /// marker.
    #[test]
    fn an_untrimmed_dendro_report_keeps_every_stream() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);

        let bytes =
            build_report(&[&src], None, None, None, "{}", Some(r#"{"events":[]}"#)).unwrap();
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(
            streams,
            ["memory/meminfo", "threads/tasks", "threads/tasks/occupants"]
        );
        assert!(!md.contains_key(KEY_REPORT));
        assert_eq!(
            md.get(KEY_EVENTS).map(String::as_str),
            Some(r#"{"events":[]}"#)
        );
    }

    /// report-save's restatement lead is the writer's restatement period.
    #[test]
    fn the_occupant_lead_is_the_writers_restatement_period() {
        assert_eq!(
            ::report_save::OCCUPANT_RESTATE_NS,
            metriken_storage::WriterConfig::default().restate_every_ns
        );
    }

    /// A ranged dendro report drops segments wholly before the range less
    /// the restatement lead, and the threads it keeps are still named.
    #[test]
    fn a_ranged_dendro_report_keeps_the_range_and_its_occupant_names() {
        use crate::dendro_copy::fixtures::{recorded, ANCHOR, SECOND};
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        // Longer than the lead, so the copy can start after the first row.
        recorded(&src, 700, true);

        // Starts after the restatement at 600 s, so a thread is named only
        // by a restatement before the range.
        let range = ::report_save::TimeRange {
            start_ns: ANCHOR + 605 * SECOND,
            end_ns: ANCHOR + 650 * SECOND,
        };
        let bytes = build_report(&[&src], None, None, Some(range), "{}", None).unwrap();
        let out = dir.path().join("report.dendro");
        std::fs::write(&out, &bytes).unwrap();

        let db = dendro::archive::Archive::open(&out).unwrap();
        let id = db.read_sources().unwrap()[0].id;
        let lead_start = range.start_ns - ::report_save::OCCUPANT_RESTATE_NS;
        for stream in db.all_streams(id).unwrap() {
            // Occupant streams hold changes and restatements, not a row a tick.
            if ::rez::occupants::table_of(&stream).is_some() {
                continue;
            }
            let segs = db
                .segments_overlapping(id, &stream, i64::MIN, i64::MAX)
                .unwrap();
            assert!(!segs.is_empty(), "{stream} kept");
            let first = segs.iter().map(|s| s.meta.first_ts).min().unwrap() as u64;
            let last = segs.iter().map(|s| s.meta.last_ts).max().unwrap() as u64;
            // Whole segments of 4 rows: up to 3 rows beyond each bound.
            assert!(
                first + 4 * SECOND > lead_start,
                "{stream} starts at {first}"
            );
            assert!(
                first <= lead_start,
                "{stream} starts at {first}, after the lead"
            );
            assert!(
                last >= range.end_ns && last < range.end_ns + 4 * SECOND,
                "{stream} ends at {last}"
            );
        }
        let rates = task_rates(&out);
        assert_eq!(rates.len(), 2, "both threads named: {rates:?}");
        assert!(rates.iter().all(|(comm, _)| !comm.is_empty()));
    }

    /// A ranged report keeps the segments overlapping the range, and a
    /// range after the data is an error.
    #[test]
    fn a_ranged_report_drops_segments_outside_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let src = upgraded("v3.rez", dir.path());
        const ANCHOR: u64 = 1_700_000_000_000_000_000;

        let after = ::report_save::TimeRange {
            start_ns: ANCHOR + 100_000_000_000,
            end_ns: ANCHOR + 200_000_000_000,
        };
        let inside = ::report_save::TimeRange {
            start_ns: ANCHOR + 2_000_000_000,
            end_ns: ANCHOR + 3_000_000_000,
        };
        let err = build_report(&[&src], None, None, Some(after), "{}", None).unwrap_err();
        assert!(err.contains("no rows"), "{err}");
        let bytes = build_report(&[&src], None, None, Some(inside), "{}", None).unwrap();
        let db = dendro::archive::Archive::open_bytes(bytes).unwrap();
        let id = db.read_sources().unwrap()[0].id;
        let rows: u64 = db
            .segments_overlapping(id, "a", i64::MIN, i64::MAX)
            .unwrap()
            .iter()
            .map(|s| s.meta.rows)
            .sum();
        assert!(rows >= 2, "the rows inside the range are kept");
    }
}
