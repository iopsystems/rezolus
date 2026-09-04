//! Save-as-Report for a `.rez` source — the server-side writer.
//!
//! A loaded `.rez` (single recording, or a 2-recording A/B) saves to a
//! trimmed `.rez` rather than a parquet or a `.parquet.ab.tar`: the report is
//! the same container as the source, which is the point of retiring the
//! tarball. It reuses the `rez_v3_rewrite` column-trim primitive
//! (`CopySpec::keep_metrics`) and embeds the selection/events/report marker
//! into the anchor recording's manifest, the same catalog-`UPDATE` shape
//! `annotate` established.
//!
//! **Server-only.** The trim machinery lives under `rez`'s `write` feature
//! (it pulls `metriken`, which has no wasm32 build), so the browser viewer
//! cannot produce a `.rez` report yet — tracked as a follow-up. Everything
//! here therefore lives in the binary, not in the wasm-safe `report-save`
//! crate.

use std::collections::BTreeSet;
use std::path::Path;

use crate::parquet_metadata::{KEY_EVENTS, KEY_REPORT, KEY_SELECTION, REPORT_VALUE_TRIMMED};

/// Build a `.rez` report from `source_path`.
///
/// When `keep_metrics` is `Some`, each table's segments are re-encoded down to
/// those metric columns (plus the structural timestamp/window sidecars) and a
/// table left with none of them is dropped — this is the trimmed report, and
/// it stamps `KEY_REPORT=trimmed` so the loader knows to open it straight to
/// the Report view. When `None`, the archive is copied whole (BLOBs verbatim)
/// and only the selection is embedded — the untrimmed "save selection" case,
/// which carries no report marker, exactly as the parquet path behaves.
///
/// `selection_json` and `events_json` are written into the ANCHOR recording's
/// manifest (recordings are numbered in catalog order, so the first is the
/// anchor the viewer maps onto the baseline slot). Returns the archive bytes.
pub fn build_rez_report(
    source_path: &Path,
    keep_metrics: Option<&BTreeSet<String>>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use crate::recorder::rez_sqlite::RezDb;
    use crate::recorder::rez_v3_rewrite::{copy_recordings_into, CopySpec};

    let src = RezDb::open(source_path)?;

    // Staged in a temp dir; the whole file is read back into memory to stream
    // as a download attachment, so it never needs a durable path.
    let staging = tempfile::tempdir().map_err(|e| format!("failed to stage a report: {e}"))?;
    let staged = staging.path().join("report.rez");

    let mut dst = RezDb::create(&staged)?;
    dst.transaction(|tx| {
        src.read_snapshot(|src| {
            let spec = CopySpec {
                keep_metrics,
                ..CopySpec::everything()
            };
            copy_recordings_into(src, tx, &spec)?;
            Ok(())
        })
    })?;

    // Embed selection / events / report marker into the anchor recording. The
    // copy is a catalog write; this is one more `UPDATE` on top of it.
    let recordings = dst.read_recordings()?;
    let anchor = recordings.first().ok_or_else(|| {
        "report has no recordings — the source archive was empty or fully trimmed away".to_string()
    })?;
    let mut metadata = anchor.meta.metadata.clone();
    metadata.insert(KEY_SELECTION.to_string(), selection_json.to_string());
    if keep_metrics.is_some() {
        metadata.insert(KEY_REPORT.to_string(), REPORT_VALUE_TRIMMED.to_string());
    }
    if let Some(events) = events_json {
        metadata.insert(KEY_EVENTS.to_string(), events.to_string());
    } else {
        // A save carrying no events must not leave a stale payload behind.
        metadata.remove(KEY_EVENTS);
    }
    dst.update_recording_metadata(anchor.id, &metadata)?;

    drop(dst);
    drop(src);

    std::fs::read(&staged).map_err(|e| format!("failed to read back the report: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
