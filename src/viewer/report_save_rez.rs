//! Save-as-Report for a `.rez` source — the server's thin adapter.
//!
//! A loaded `.rez` (single recording, or a 2-recording A/B) saves to a trimmed
//! `.rez` rather than a parquet or a `.parquet.ab.tar`. The actual assembly is
//! the shared, reader-available `report_save::build_rez_report_from_rez` (so
//! the browser viewer runs the same code); this wrapper just reads the source
//! file into bytes, since the server has a path and the shared crate works on
//! bytes. A v2 (tar) source is not supported here — `open_bytes` needs a v3
//! SQLite image; upgrade the archive first.

use std::collections::BTreeSet;
use std::path::Path;

/// Build a `.rez` report from `source_path` — see
/// `report_save::build_rez_report_from_rez` for the semantics (trim when
/// `keep_metrics` is `Some`, embed selection/events, stamp the report marker).
pub fn build_rez_report(
    source_path: &Path,
    keep_metrics: Option<&BTreeSet<String>>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(source_path)
        .map_err(|e| format!("failed to read {}: {e}", source_path.display()))?;
    ::report_save::build_rez_report_from_rez(&bytes, keep_metrics, selection_json, events_json)
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
        let bytes = build_rez_report(&src, Some(&keep), r#"{"entries":[]}"#, None).unwrap();

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

        let bytes = build_rez_report(&src, None, r#"{"entries":[]}"#, None).unwrap();
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

        let bytes = build_rez_report(&src, None, "{}", None).unwrap();
        let out = dir.path().join("report.rez");
        std::fs::write(&out, &bytes).unwrap();
        let db = RezDb::open(&out).unwrap();
        let md = &db.read_recordings().unwrap()[0].meta.metadata;
        assert!(
            !md.contains_key(KEY_EVENTS),
            "a save with no events drops the stale key"
        );
    }
}
