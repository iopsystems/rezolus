//! Parquet metadata extraction and dashboard regeneration.
//!
//! All file-level inspection (systeminfo, selection, multi-node info,
//! service-extension lookup, file checksum) lives here, plus the
//! `regenerate_dashboards` orchestrator that re-derives the section map
//! whenever a capture is attached/detached.

use std::path::Path;

use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;
use tracing::warn;

#[cfg(feature = "live-mode")]
use super::capture_registry::CaptureId;
#[cfg(feature = "live-mode")]
use super::promql::{self, QueryEngine};
use super::state::{AppState, LazySectionStore};
#[cfg(feature = "live-mode")]
use super::tsdb::Tsdb;
use ::dashboard::{self, CategoryExtension, ServiceExtension, TemplateRegistry};

/// Read systeminfo, selection, and the full key-value map (with
/// pre-computed multi-node enrichment) from a parquet file's metadata.
pub fn extract_parquet_metadata(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    std::fs::File::open(path)
        .ok()
        .and_then(|f| {
            let reader = SerializedFileReader::new(f).ok()?;
            let kv = reader.metadata().file_metadata().key_value_metadata()?;
            let sysinfo = kv
                .iter()
                .find(|kv| kv.key == "systeminfo")
                .and_then(|kv| kv.value.clone());
            let sel = kv
                .iter()
                .find(|kv| kv.key == "selection")
                .and_then(|kv| kv.value.clone());

            let mut map = serde_json::Map::new();
            for pair in kv {
                if let Some(ref val) = pair.value {
                    let json_val = serde_json::from_str(val)
                        .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
                    map.insert(pair.key.clone(), json_val);
                }
            }
            enrich_with_multi_node_info(&mut map);
            let file_meta = serde_json::to_string(&serde_json::Value::Object(map)).ok();

            Some((sysinfo, sel, file_meta))
        })
        .unwrap_or((None, None, None))
}

/// Pre-compute multi-node info (nodes list, version map, service
/// instances) from `per_source_metadata` so the JS frontend doesn't
/// re-parse it.
fn enrich_with_multi_node_info(map: &mut serde_json::Map<String, serde_json::Value>) {
    let psm = match map.get("per_source_metadata").and_then(|v| v.as_object()) {
        Some(psm) => psm.clone(),
        None => return,
    };

    let mut nodes = Vec::new();
    let mut node_versions = serde_json::Map::new();
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (sub_key, entry) in rez_group {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let node_name = obj.get("node").and_then(|v| v.as_str()).unwrap_or(sub_key);
            if !nodes.contains(&node_name.to_string()) {
                nodes.push(node_name.to_string());
            }
            if let Some(version) = obj.get("version").and_then(|v| v.as_str()) {
                node_versions.insert(
                    node_name.to_string(),
                    serde_json::Value::String(version.to_string()),
                );
            }
        }
    }

    let mut service_instances = serde_json::Map::new();
    for (source, group) in &psm {
        if source == "rezolus" {
            continue;
        }
        let Some(group_obj) = group.as_object() else {
            continue;
        };
        let mut instances = Vec::new();
        for (sub_key, entry) in group_obj {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let instance_id = obj
                .get("instance")
                .and_then(|v| v.as_str())
                .unwrap_or(sub_key);
            let node = obj.get("node").and_then(|v| v.as_str());
            let mut inst = serde_json::Map::new();
            inst.insert(
                "id".into(),
                serde_json::Value::String(instance_id.to_string()),
            );
            inst.insert(
                "node".into(),
                node.map(|n| serde_json::Value::String(n.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            );
            instances.push(serde_json::Value::Object(inst));
        }
        if !instances.is_empty() {
            service_instances.insert(source.clone(), serde_json::Value::Array(instances));
        }
    }

    map.insert(
        "nodes".into(),
        serde_json::Value::Array(nodes.into_iter().map(serde_json::Value::String).collect()),
    );
    if !node_versions.is_empty() {
        map.insert(
            "node_versions".into(),
            serde_json::Value::Object(node_versions),
        );
    }
    if !service_instances.is_empty() {
        map.insert(
            "service_instances".into(),
            serde_json::Value::Object(service_instances),
        );
    }
}

/// When the parquet has multi-node systeminfo (>1 rezolus node), assemble
/// it into a JSON object keyed by node name. Returns `None` for
/// single-node files; the caller falls back to the flat top-level
/// `systeminfo` key.
pub fn build_multinode_systeminfo(path: &Path) -> Option<String> {
    use crate::parquet_metadata::KEY_PER_SOURCE_METADATA;

    let f = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(f).ok()?;
    let kv = reader.metadata().file_metadata().key_value_metadata()?;

    let psm_json = kv
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_ref())?;

    let psm: serde_json::Map<String, serde_json::Value> = serde_json::from_str(psm_json).ok()?;

    let mut nodes = serde_json::Map::new();
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (node_key, entry) in rez_group {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let Some(sysinfo_val) = obj.get("systeminfo") else {
                continue;
            };
            let node_name = obj
                .get("node")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| node_key.clone());
            nodes.insert(node_name, sysinfo_val.clone());
        }
    }

    if nodes.len() > 1 {
        serde_json::to_string(&serde_json::Value::Object(nodes)).ok()
    } else {
        None
    }
}

/// SHA-256 of the parquet file body, excluding footer metadata so the
/// digest is stable across selection annotations. Layout:
/// `[magic 4B] [row groups] [footer] [footer_len 4B] [magic 4B]`. We
/// hash `[0, file_len - 8 - footer_len)`.
pub fn compute_file_checksum(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    (|| -> Option<String> {
        let mut f = std::fs::File::open(path).ok()?;
        let file_len = f.metadata().ok()?.len();
        if file_len < 12 {
            return None;
        }
        f.seek(SeekFrom::End(-8)).ok()?;
        let mut tail = [0u8; 4];
        f.read_exact(&mut tail).ok()?;
        let footer_len = u32::from_le_bytes(tail) as u64;
        let data_end = file_len.checked_sub(8 + footer_len)?;
        f.seek(SeekFrom::Start(0)).ok()?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut remaining = data_end;
        while remaining > 0 {
            let to_read = (remaining as usize).min(buf.len());
            match f.read(&mut buf[..to_read]) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                    remaining -= n as u64;
                }
                Err(e) => {
                    warn!("failed to read file for checksum: {e}");
                    return None;
                }
            }
        }
        Some(
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        )
    })()
}

/// Service-extension lookup: by precedence,
/// (1) top-level `service_queries`,
/// (2) `per_source_metadata.<source>.service_queries`,
/// (3) built-in template for known sources.
///
/// The returned source keys come purely from the parquet's metadata —
/// CLI legend labels are display-only and don't influence template
/// binding.
pub fn extract_service_extension_metadata(
    path: &Path,
    registry: &TemplateRegistry,
) -> Vec<(String, ServiceExtension)> {
    use crate::parquet_metadata::{
        KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, NESTED_SERVICE_QUERIES,
    };

    let mut results = Vec::new();

    let Ok(f) = std::fs::File::open(path) else {
        return results;
    };
    let Ok(reader) = SerializedFileReader::new(f) else {
        return results;
    };
    let Some(kv) = reader.metadata().file_metadata().key_value_metadata() else {
        return results;
    };

    // 1. Top-level service_queries (written by `parquet annotate`).
    if let Some(sq_json) = kv
        .iter()
        .find(|kv| kv.key == KEY_SERVICE_QUERIES)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(ext) = serde_json::from_str::<ServiceExtension>(sq_json) {
            let source = kv
                .iter()
                .find(|kv| kv.key == KEY_SOURCE)
                .and_then(|kv| kv.value.as_deref())
                .unwrap_or(&ext.service_name);
            results.push((source.to_string(), ext));
        }
    }

    // 2. Nested under per_source_metadata (combined files).
    if let Some(metadata_json) = kv
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(metadata_map) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(metadata_json)
        {
            for (source, group_val) in &metadata_map {
                if results.iter().any(|(s, _)| s == source) {
                    continue;
                }
                if let Some(group) = group_val.as_object() {
                    for (_sub_key, entry) in group {
                        if let Some(sq) = entry.get(NESTED_SERVICE_QUERIES) {
                            if let Ok(ext) = serde_json::from_value::<ServiceExtension>(sq.clone())
                            {
                                results.push((source.clone(), ext));
                                break; // one extension per source
                            }
                        }
                    }
                }
            }

            // 3a. No service_queries — fall back to built-in templates.
            for source in metadata_map.keys() {
                if results.iter().any(|(s, _)| s == source) {
                    continue;
                }
                if let Some(ext) = registry.get(source) {
                    results.push((source.clone(), ext.clone()));
                }
            }
        }
    }

    // 3b. No per_source_metadata — check the top-level source key.
    if results.is_empty() {
        if let Some(source) = kv
            .iter()
            .find(|kv| kv.key == KEY_SOURCE)
            .and_then(|kv| kv.value.as_deref())
        {
            if let Some(ext) = registry.get(source) {
                results.push((source.to_string(), ext.clone()));
            }
        }
    }

    results
}

/// Run each KPI's PromQL against the loaded TSDB so the dashboard can
/// hide KPIs whose queries return no data (e.g. zero-traffic histograms).
#[cfg(feature = "live-mode")]
pub fn validate_service_extensions(tsdb: &Tsdb, exts: &mut [(String, ServiceExtension)]) {
    let engine = QueryEngine::new(tsdb);
    let (start, end) = engine.get_time_range();

    for (_source, ext) in exts.iter_mut() {
        for kpi in &mut ext.kpis {
            let query = kpi.effective_query();
            let has_data = match engine.query_range(&query, start, end, 1.0) {
                Ok(result) => match &result {
                    promql::QueryResult::Vector { result } => !result.is_empty(),
                    promql::QueryResult::Matrix { result } => !result.is_empty(),
                    promql::QueryResult::Scalar { .. } => true,
                    promql::QueryResult::HistogramHeatmap { result } => !result.data.is_empty(),
                },
                Err(_) => false,
            };
            kpi.available = has_data;
        }
    }
}

/// Resolve the active category for a regen pass. Activation requires
/// `state.category_name` to be Some, two service refs attached, and each
/// ref's source name to appear in the category's `members`. Returns
/// None when any of those fail — the caller falls back to per-member
/// rendering. CLI startup ran stricter checks; silent fall-back here
/// only happens at runtime (e.g. mid-session detach).
pub fn lookup_category<'a>(
    state: &AppState,
    registry: &'a TemplateRegistry,
    service_refs: &[(&str, &ServiceExtension)],
) -> Option<(&'a str, &'a CategoryExtension)> {
    let cat_name = state.category_name.read().clone()?;
    if service_refs.len() != 2 {
        return None;
    }
    let category = registry.get_category(&cat_name)?;
    for (source, _) in service_refs {
        if !category.members.iter().any(|m| m == source) {
            return None;
        }
    }
    Some((category.service_name.as_str(), category))
}

/// Regenerate the section map from the currently attached captures.
/// Called at CLI startup after the experiment attaches, and on every
/// HTTP attach/detach so the section list stays in sync.
pub fn regenerate_dashboards(state: &AppState) {
    if state.is_trimmed_report() {
        // Skip section construction — a trimmed report has columns only
        // for the saved selection's queries, so rezolus / service /
        // Query Explorer sections would all render empty.
        let filesize = state
            .parquet_path
            .read()
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));
        *state.sections.write() = LazySectionStore::new(::dashboard::dashboard::DashboardContext {
            filesize,
            ..Default::default()
        });
        return;
    }

    let registry = &state.templates;
    let baseline_path = state.parquet_path.read().clone();
    // Prefer the HTTP-owned temp path; fall back to the CLI-supplied
    // user path. Stored in separate fields so `detach_experiment` can
    // safely delete only server-owned temp files.
    let experiment_path = state
        .experiment_parquet_path
        .read()
        .clone()
        .or_else(|| state.cli_experiment_path.read().clone());

    // Validate baseline exts against the baseline tsdb and experiment
    // exts against the experiment tsdb so a KPI present only in one
    // recording isn't wrongly marked unavailable.
    let mut baseline_exts: Vec<(String, ServiceExtension)> = baseline_path
        .as_ref()
        .map(|p| extract_service_extension_metadata(p, registry))
        .unwrap_or_default();
    let mut experiment_exts: Vec<(String, ServiceExtension)> = experiment_path
        .as_ref()
        .map(|p| extract_service_extension_metadata(p, registry))
        .unwrap_or_default();

    // Validate against the Tsdb when one is loaded (live mode). SQL-
    // backed captures skip this — the SQL-aware validator lands once
    // service-extension templates carry `sql` strings (plan stage 8).
    // Until then SQL captures show every KPI as `available: true`.
    #[cfg(feature = "live-mode")]
    {
        if let Some(baseline_handle) = state.baseline_tsdb() {
            let baseline_data = baseline_handle.read();
            validate_service_extensions(&baseline_data, &mut baseline_exts);
        }
        if !experiment_exts.is_empty() {
            if let Some(experiment_handle) = state.captures.get(CaptureId::Experiment) {
                let experiment_data = experiment_handle.read();
                validate_service_extensions(&experiment_data, &mut experiment_exts);
            }
        }
    }

    let mut service_exts = baseline_exts;
    service_exts.extend(experiment_exts);

    let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
    let category = lookup_category(state, registry, &service_refs);

    let filesize = baseline_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));

    let context = dashboard::dashboard::build_dashboard_context(filesize, &service_refs, category);
    *state.sections.write() = LazySectionStore::new(context);
}

#[cfg(all(test, feature = "live-mode"))]
mod report_mode_tests {
    use super::*;
    use ::dashboard::TemplateRegistry;
    use metriken_query::Tsdb;

    #[test]
    fn regenerate_returns_empty_sections_for_trimmed_report() {
        let state = AppState::new(Tsdb::default(), TemplateRegistry::empty());
        *state.trimmed_report_marker.write() = Some("trimmed".to_string());
        regenerate_dashboards(&state);
        let sections = state.sections.read();
        assert!(
            sections.is_empty(),
            "trimmed report should have no sections"
        );
    }
}
