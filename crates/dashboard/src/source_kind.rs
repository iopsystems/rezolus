//! Per-source classification shared by both viewer backends (the axum
//! server in `src/viewer/` and the WASM crate in `crates/viewer/`).
//!
//! This is a pure helper: it produces the `Vec<SourceEntry>` the passive
//! dashboard renderer consumes; it does not itself render. It lives in the
//! `dashboard` crate — not the viewer — because the WASM crate cannot depend
//! on the binary crate, and both backends must classify a loaded recording
//! identically. A change here lands for both viewers at once (see the
//! `viewer-parity` skill).

use crate::dashboard::SourceEntry;
use std::collections::HashSet;

// Format keys owned by the binary's `src/parquet_metadata.rs`. Duplicated as
// literals here because the `dashboard` crate can't depend on the binary
// crate (the WASM viewer builds it standalone). These are the on-disk parquet
// footer contract, not internal names — they change rarely and in lockstep.
const KEY_SOURCE: &str = "source";
const KEY_PER_SOURCE_METADATA: &str = "per_source_metadata";
const NESTED_SAMPLER_STATUS: &str = "sampler_status";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Rezolus,
    Service,
    Simple,
}

/// Cross-platform Rezolus self-telemetry (rezolus/rusage sampler). Present on
/// both Linux and macOS. Deliberately excludes rezolus_bpf_* (Linux/eBPF-only,
/// absent on macOS — including them would misclassify macOS recordings).
pub const REZOLUS_SELF_ANCHORS: &[&str] = &[
    "rezolus_cpu_usage",
    "rezolus_memory_usage_resident_set_size",
    "rezolus_rusage",
];

pub fn detect_source_kind(
    source: &str,
    has_sampler_status: bool,
    has_template: bool,
    metric_names: &[String],
) -> SourceKind {
    // Tier 1: metadata markers (source tag, sampler_status). Drop spoofable systeminfo.
    if source == "rezolus" || has_sampler_status {
        return SourceKind::Rezolus;
    }
    if has_template {
        return SourceKind::Service;
    }
    // Tier 2: cross-platform self-sampler fingerprint.
    if metric_names
        .iter()
        .any(|n| REZOLUS_SELF_ANCHORS.contains(&n.as_str()))
    {
        return SourceKind::Rezolus;
    }
    SourceKind::Simple
}

/// Resolve the display/section name for a source. Rezolus: node else "rezolus".
/// Simple: explicit source -> filename stem -> "metrics".
pub fn resolve_source_name(
    kind: SourceKind,
    source: &str,
    node: Option<&str>,
    filename_stem: Option<&str>,
) -> String {
    match kind {
        SourceKind::Rezolus => node
            .filter(|s| !s.is_empty())
            .unwrap_or("rezolus")
            .to_string(),
        SourceKind::Service => source.to_string(),
        SourceKind::Simple => {
            if !source.is_empty() {
                source.to_string()
            } else if let Some(stem) = filename_stem.filter(|s| !s.is_empty()) {
                stem.to_string()
            } else {
                "metrics".to_string()
            }
        }
    }
}

/// Classify every non-service source present in `file_metadata` into
/// `SourceEntry` descriptors for the dashboard nav. Service sources are
/// excluded — they already appear via the service extensions.
///
/// `file_metadata` is the parsed file-level key/value map (server: read from
/// the parquet footer; WASM: from `reader.file_metadata()`). `metric_names`
/// is the union of counter/gauge/histogram names, for the self-sampler
/// fingerprint. `service_names` are the sources already covered by a service
/// template. `filename_stem` is the fallback display name for an unnamed
/// simple capture.
///
/// Single-source files are fully supported. Combined files make a best-effort
/// pass over `per_source_metadata` keys with the information available.
pub fn classify_sources(
    file_metadata: &serde_json::Value,
    metric_names: &[String],
    service_names: &HashSet<&str>,
    filename_stem: Option<&str>,
) -> Vec<SourceEntry> {
    let mut entries: Vec<SourceEntry> = Vec::new();

    // Combined file: per_source_metadata keys enumerate sources.
    if let Some(psm) = file_metadata
        .get(KEY_PER_SOURCE_METADATA)
        .and_then(|v| v.as_object())
    {
        for (source, group_val) in psm {
            // Each group is a map of sub-keys → entry objects.
            let group = group_val.as_object();
            let has_sampler_status = group.is_some_and(|g| {
                g.values().any(|entry| {
                    entry
                        .as_object()
                        .is_some_and(|obj| obj.contains_key(NESTED_SAMPLER_STATUS))
                })
            });
            let node = group.and_then(|g| {
                g.values().find_map(|entry| {
                    entry
                        .as_object()
                        .and_then(|obj| obj.get("node"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
            });
            let has_template = service_names.contains(source.as_str());

            let kind = detect_source_kind(source, has_sampler_status, has_template, metric_names);
            if kind == SourceKind::Service {
                continue;
            }
            let name = resolve_source_name(kind, source, node.as_deref(), filename_stem);
            entries.push(SourceEntry {
                name,
                is_rezolus: kind == SourceKind::Rezolus,
            });
        }
    } else {
        // Single-source file: use the top-level `source` key. Sampler status
        // is not present at the top level in single-source files.
        let source = file_metadata
            .get(KEY_SOURCE)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let has_template = service_names.contains(source);

        let kind = detect_source_kind(source, false, has_template, metric_names);
        if kind != SourceKind::Service {
            let name = resolve_source_name(kind, source, None, filename_stem);
            entries.push(SourceEntry {
                name,
                is_rezolus: kind == SourceKind::Rezolus,
            });
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn metadata_marker_source_rezolus() {
        assert_eq!(
            detect_source_kind("rezolus", false, false, &[]),
            SourceKind::Rezolus
        );
    }

    #[test]
    fn metadata_marker_sampler_status() {
        assert_eq!(
            detect_source_kind("anything", true, false, &[]),
            SourceKind::Rezolus
        );
    }

    #[test]
    fn template_makes_service() {
        assert_eq!(
            detect_source_kind("llm-perf", false, true, &[]),
            SourceKind::Service
        );
    }

    #[test]
    fn self_sampler_fingerprint_linux() {
        let m = names(&["rezolus_cpu_usage", "rezolus_rusage", "cpu_usage"]);
        assert_eq!(
            detect_source_kind("", false, false, &m),
            SourceKind::Rezolus
        );
    }

    #[test]
    fn self_sampler_fingerprint_macos_no_bpf() {
        // macOS recording: rusage self-metrics present, NO rezolus_bpf_* at all.
        let m = names(&[
            "rezolus_cpu_usage",
            "rezolus_memory_usage_resident_set_size",
        ]);
        assert_eq!(
            detect_source_kind("", false, false, &m),
            SourceKind::Rezolus
        );
    }

    #[test]
    fn foreign_metrics_are_simple() {
        let m = names(&["http_requests_total", "queue_depth"]);
        assert_eq!(detect_source_kind("", false, false, &m), SourceKind::Simple);
    }

    #[test]
    fn name_resolution() {
        assert_eq!(
            resolve_source_name(SourceKind::Rezolus, "rezolus", Some("node7"), None),
            "node7"
        );
        assert_eq!(
            resolve_source_name(SourceKind::Rezolus, "rezolus", None, Some("x")),
            "rezolus"
        );
        assert_eq!(
            resolve_source_name(SourceKind::Simple, "svc", None, Some("cap")),
            "svc"
        );
        assert_eq!(
            resolve_source_name(SourceKind::Simple, "", None, Some("cap")),
            "cap"
        );
        assert_eq!(
            resolve_source_name(SourceKind::Simple, "", None, None),
            "metrics"
        );
    }

    // ── classify_sources: the shared behavior both backends now call ──

    fn svc<'a>(names: &[&'a str]) -> HashSet<&'a str> {
        names.iter().copied().collect()
    }

    #[test]
    fn single_source_simple_capture_yields_one_source_entry() {
        // A non-Rezolus single-source parquet (source="hub", foreign metrics)
        // must produce exactly one non-rezolus SourceEntry named "hub" — this
        // is the section that was silently missing in the WASM viewer.
        let meta = serde_json::json!({ "source": "hub" });
        let metrics = names(&["hub_heartbeat"]);
        let out = classify_sources(&meta, &metrics, &svc(&[]), Some("hub.heartbeat"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "hub");
        assert!(!out[0].is_rezolus);
    }

    #[test]
    fn single_source_rezolus_is_rezolus_entry() {
        let meta = serde_json::json!({ "source": "rezolus" });
        let metrics = names(&["rezolus_cpu_usage", "cpu_usage"]);
        let out = classify_sources(&meta, &metrics, &svc(&[]), None);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_rezolus);
    }

    #[test]
    fn single_source_fingerprint_only_is_rezolus() {
        // No explicit source tag, but self-sampler metrics present.
        let meta = serde_json::json!({});
        let metrics = names(&["rezolus_rusage", "scheduler_runqueue_latency"]);
        let out = classify_sources(&meta, &metrics, &svc(&[]), Some("capture"));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_rezolus);
    }

    #[test]
    fn service_source_is_excluded() {
        // A source covered by a service template is rendered via its service
        // section, not a source: entry.
        let meta = serde_json::json!({ "source": "vllm" });
        let metrics = names(&["vllm_requests"]);
        let out = classify_sources(&meta, &metrics, &svc(&["vllm"]), None);
        assert!(out.is_empty());
    }

    #[test]
    fn combined_file_classifies_each_source() {
        // per_source_metadata with a rezolus group and a foreign group:
        // rezolus → is_rezolus entry (shows built-ins), foreign → source entry.
        let meta = serde_json::json!({
            "per_source_metadata": {
                "rezolus": { "node-a": { "sampler_status": {}, "node": "node-a" } },
                "hub": { "inst-1": { "instance": "inst-1" } }
            }
        });
        let metrics = names(&["hub_heartbeat", "cpu_usage"]);
        let mut out = classify_sources(&meta, &metrics, &svc(&[]), None);
        out.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(out.len(), 2);
        // node-a: rezolus (via sampler_status marker), named by node.
        let rez = out.iter().find(|e| e.is_rezolus).expect("a rezolus entry");
        assert_eq!(rez.name, "node-a");
        // hub: simple, named by source key.
        let simple = out.iter().find(|e| !e.is_rezolus).expect("a simple entry");
        assert_eq!(simple.name, "hub");
    }

    #[test]
    fn empty_metadata_yields_no_entries_when_nothing_identifiable() {
        // No source key, no per_source_metadata, no fingerprint, no stem →
        // still classify the (empty-source) single-source path as one Simple
        // entry named by the stem fallback.
        let meta = serde_json::json!({});
        let metrics = names(&["queue_depth"]);
        let out = classify_sources(&meta, &metrics, &svc(&[]), Some("mycap"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "mycap");
        assert!(!out[0].is_rezolus);
    }
}
