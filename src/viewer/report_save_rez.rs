//! Save-as-Report for a `.rez` source — the server's thin adapter.
//!
//! A loaded `.rez` (single recording, or a 2-recording A/B) saves to a trimmed
//! `.rez` rather than a parquet or a `.parquet.ab.tar`. The actual assembly is
//! the shared, reader-available `report_save::build_rez_report_from_rez` (so
//! the browser viewer runs the same code); this wrapper just reads the source
//! file into bytes, since the server has a path and the shared crate works on
//! bytes. A v2 (tar) source is not supported here — `open_bytes` needs a v3
//! SQLite image; upgrade the archive first. A dendro source saves to a dendro
//! report, the same copy `recording filter` makes, with the markers stamped on
//! its first source.

use std::collections::BTreeSet;
use std::path::Path;

/// Build a `.rez` report from `source_path` — see
/// `report_save::build_rez_report_from_rez` for the semantics (trim when
/// `keep_metrics` is `Some`, keep the segments overlapping `range`, embed
/// selection/events, stamp the report marker).
pub fn build_rez_report(
    source_path: &Path,
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<::report_save::TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(source_path)
        .map_err(|e| format!("failed to read {}: {e}", source_path.display()))?;
    ::report_save::build_rez_report_from_rez(
        &bytes,
        keep_metrics,
        range,
        selection_json,
        events_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parquet_metadata::{KEY_EVENTS, KEY_REPORT, KEY_SELECTION, REPORT_VALUE_TRIMMED};
    use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
    use crate::recorder::rez_sqlite::RezDb;
    use std::collections::BTreeSet;

    /// A trimmed `.rez` report embeds the selection and stamps the report
    /// marker on the anchor, and drops tables holding none of the kept metrics
    /// (this fixture names each sampler's single metric by its index).
    #[test]
    fn trimmed_report_embeds_selection_marker_and_drops_unkept_tables() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.rez");
        populated_v3_rez(&src, "baseline", &["cpu_usage", "scheduler"], 6);

        let keep: BTreeSet<String> = ["0".to_string()].into_iter().collect();
        let bytes = build_rez_report(&src, Some(&keep), None, r#"{"entries":[]}"#, None).unwrap();

        let out = dir.path().join("report.rez");
        std::fs::write(&out, &bytes).unwrap();
        let db = RezDb::open(&out).unwrap();
        let recs = db.read_recordings().unwrap();
        assert_eq!(recs.len(), 1);
        let md = &recs[0].meta.metadata;
        assert_eq!(
            md.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[]}"#)
        );
        assert_eq!(
            md.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );
        assert_eq!(
            db.all_samplers(recs[0].id).unwrap(),
            vec!["cpu_usage".to_string()],
            "only the table holding kept metric \"0\" survives"
        );
    }

    /// An untrimmed save (keep_metrics = None) embeds the selection but carries
    /// no report marker and copies every table — matching the parquet path,
    /// where only a trim stamps `KEY_REPORT`.
    #[test]
    fn untrimmed_report_embeds_selection_without_report_marker() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.rez");
        populated_v3_rez(&src, "baseline", &["cpu_usage", "scheduler"], 6);

        let bytes = build_rez_report(&src, None, None, r#"{"entries":[]}"#, None).unwrap();
        let out = dir.path().join("report.rez");
        std::fs::write(&out, &bytes).unwrap();
        let db = RezDb::open(&out).unwrap();
        let recs = db.read_recordings().unwrap();
        let md = &recs[0].meta.metadata;
        assert!(md.contains_key(KEY_SELECTION), "selection embedded");
        assert!(
            !md.contains_key(KEY_REPORT),
            "an untrimmed save carries no report marker"
        );
        assert_eq!(
            db.all_samplers(recs[0].id).unwrap().len(),
            2,
            "every table is copied"
        );
    }

    /// A save whose payload has no events must clear any events the source
    /// carried, the same way the parquet footer path (`embed_selection`) does.
    #[test]
    fn a_save_with_no_events_clears_a_stale_events_key() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.rez");
        populated_v3_rez(&src, "baseline", &["cpu_usage"], 4);
        {
            let db = RezDb::open(&src).unwrap();
            let recs = db.read_recordings().unwrap();
            let mut md = recs[0].meta.metadata.clone();
            md.insert(
                KEY_EVENTS.to_string(),
                r#"{"events":[{"timestamp":1,"description":"x"}]}"#.to_string(),
            );
            db.update_recording_metadata(recs[0].id, &md).unwrap();
        }

        let bytes = build_rez_report(&src, None, None, "{}", None).unwrap();
        let out = dir.path().join("report.rez");
        std::fs::write(&out, &bytes).unwrap();
        let db = RezDb::open(&out).unwrap();
        let md = &db.read_recordings().unwrap()[0].meta.metadata;
        assert!(
            !md.contains_key(KEY_EVENTS),
            "a save with no events drops the stale key"
        );
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A dendro report's streams and its first source's metadata.
    fn dendro_report(bytes: &[u8]) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
        let db = dendro::archive::Archive::open_bytes(bytes.to_vec()).unwrap();
        let sources = db.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        let mut streams = db.all_streams(sources[0].id).unwrap();
        streams.sort();
        (
            streams,
            sources[0].meta.metadata.clone().into_iter().collect(),
        )
    }

    /// `sum by (comm) (rate(task_ops[3s]))` over the archive at `path`.
    fn task_rates(path: &std::path::Path) -> Vec<(String, Vec<(f64, f64)>)> {
        use metriken_query::MetricsSource;
        let r = crate::rez_reader::RezReader::open_recordings(
            path,
            metriken_query::BufferPool::new(64 * 1024 * 1024),
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

    /// A trimmed report of a dendro source is a dendro archive: the long
    /// table stays long with its occupant stream, the untouched table goes,
    /// the markers sit on the source, and the kept metric answers the same.
    /// The source is unfinalized, so its last rows come from the WAL tail.
    #[test]
    fn a_dendro_source_saves_a_trimmed_dendro_report() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, false);

        let bytes = build_rez_report(
            &src,
            Some(&set(&["task_ops"])),
            None,
            r#"{"entries":[]}"#,
            None,
        )
        .unwrap();
        assert!(metriken_archive::DendroCatalog::is_archive_bytes(&bytes));
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(
            streams,
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
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
            metriken_query::BufferPool::new(64 * 1024 * 1024),
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

        let bytes = build_rez_report(&src, Some(&set(&["mem_free"])), None, "{}", None).unwrap();
        let (streams, _) = dendro_report(&bytes);
        assert_eq!(streams, vec!["memory/meminfo".to_string()]);
    }

    /// An untrimmed dendro report copies every stream and carries no report
    /// marker, as the `.rez` path does.
    #[test]
    fn an_untrimmed_dendro_report_keeps_every_stream() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);

        let bytes = build_rez_report(&src, None, None, "{}", Some(r#"{"events":[]}"#)).unwrap();
        let (streams, md) = dendro_report(&bytes);
        assert_eq!(
            streams,
            vec![
                "memory/meminfo".to_string(),
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
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
            metriken_archive::WriterConfig::default().restate_every_ns
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
        let bytes = build_rez_report(&src, None, Some(range), "{}", None).unwrap();
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

    /// A ranged `.rez` report keeps the segments overlapping the range, and a
    /// range after the data is an error, as it is for a parquet.
    #[test]
    fn a_ranged_rez_report_drops_segments_outside_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.rez");
        populated_v3_rez(&src, "baseline", &["cpu_usage"], 6);
        const ANCHOR: u64 = 1_700_000_000_000_000_000;

        let after = ::report_save::TimeRange {
            start_ns: ANCHOR + 100_000_000_000,
            end_ns: ANCHOR + 200_000_000_000,
        };
        let inside = ::report_save::TimeRange {
            start_ns: ANCHOR + 2_000_000_000,
            end_ns: ANCHOR + 3_000_000_000,
        };
        let err = build_rez_report(&src, None, Some(after), "{}", None).unwrap_err();
        assert!(err.contains("no rows"), "{err}");
        let rows = |range| {
            let bytes = build_rez_report(&src, None, Some(range), "{}", None).unwrap();
            let db = RezDb::open_bytes(bytes).unwrap();
            let id = db.read_recordings().unwrap()[0].id;
            db.segments_overlapping(id, "cpu_usage", 0, u64::MAX)
                .unwrap()
                .iter()
                .map(|s| s.meta.rows)
                .sum::<u64>()
        };
        assert!(rows(inside) >= 2, "the rows inside the range are kept");
    }
}
