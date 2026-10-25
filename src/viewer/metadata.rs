//! Capture metadata (service-extension lookup, file checksum) and the
//! `regenerate_dashboards` orchestrator that re-derives the section map
//! whenever a capture is attached/detached.

use std::collections::HashMap;
use std::path::Path;

use tracing::warn;

use super::capture_registry::CaptureId;
use metriken_query::{MetricsSource, QueryResult};

use super::state::{AppState, LazySectionStore};
use ::dashboard::section_template::SectionTemplate;
use ::dashboard::source_templates::TemplatePreference;
use ::dashboard::{self, CategoryExtension, ServiceExtension, TemplateRegistry};
use std::sync::Arc;

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

/// The KPI templates for one source's metadata map
/// (`dashboard::source_templates::service_extensions_from_metadata`, which
/// says the order), with each notice logged as a warning once per process:
/// the terminal UI regenerates its dashboards every tick, and a template that
/// did not parse the first time does not parse the next.
pub fn service_extensions_from_metadata(
    metadata: &HashMap<String, String>,
    registry: &TemplateRegistry,
    preference: TemplatePreference,
) -> Vec<(String, ServiceExtension)> {
    let (exts, notices) = ::dashboard::source_templates::service_extensions_from_metadata(
        metadata, registry, preference,
    );
    for notice in notices {
        warn_once(notice);
    }
    exts
}

/// Log `notice` as a warning unless this process has already logged it.
pub(crate) fn warn_once(notice: String) {
    if first_time(&notice) {
        warn!("{notice}");
    }
}

/// Whether this process has not seen `notice` before; records it.
fn first_time(notice: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(notice.to_string())
}

/// The section template a capture's built-in sections render from: the
/// `dashboard` template in its metadata, or the embedded one
/// (`dashboard::source_templates::resolve_dashboard_template`). Why a carried
/// template was refused is logged once per process.
pub fn capture_dashboard_template(
    data: &dyn MetricsSource,
    preference: TemplatePreference,
) -> Arc<SectionTemplate> {
    let chosen = ::dashboard::source_templates::resolve_dashboard_template(
        &data.file_metadata(),
        preference,
    );
    if let Some(notice) = chosen.notice {
        warn_once(notice);
    }
    chosen.template
}

/// The section context of a live agent's capture: the built-in sections
/// alone, from the template the agent's handshake carried when `data` has it.
pub fn live_dashboard_context(
    data: &dyn MetricsSource,
    preference: TemplatePreference,
) -> ::dashboard::dashboard::DashboardContext {
    ::dashboard::dashboard::build_template_context(
        capture_dashboard_template(data, preference),
        None,
        &[],
        None,
        &[],
    )
}

/// The KPI templates for a loaded capture, from its own metadata.
pub fn capture_service_extensions(
    data: &dyn MetricsSource,
    registry: &TemplateRegistry,
    preference: TemplatePreference,
) -> Vec<(String, ServiceExtension)> {
    service_extensions_from_metadata(&data.file_metadata(), registry, preference)
}

/// Run each KPI's PromQL against the loaded TSDB so the dashboard can
/// hide KPIs whose queries return no data (e.g. zero-traffic histograms).
pub fn validate_service_extensions(
    data: &dyn MetricsSource,
    exts: &mut [(String, ServiceExtension)],
) {
    let (start, end) = data.time_range().unwrap_or((0.0, 0.0));

    for (_source, ext) in exts.iter_mut() {
        for kpi in &mut ext.kpis {
            let query = kpi.effective_query();
            let has_data = match data.query_range(&query, start, end, 1.0) {
                Ok(result) => match &result {
                    QueryResult::Vector { result } => !result.is_empty(),
                    QueryResult::Matrix { result } => !result.is_empty(),
                    QueryResult::Scalar { .. } => true,
                    QueryResult::HistogramHeatmap { result } => !result.data.is_empty(),
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

/// Rebuild the sections of a followed combined archive whose recording set
/// changed since they were built (an endpoint that came up after the open
/// added a recording), and say so: the page lists sections when it loads,
/// so the new recording's appear on a reload. A no-op otherwise.
pub fn refresh_combined_members(state: &AppState) {
    let Some((live, built)) = state.combined_live.lock().clone() else {
        return;
    };
    let (labels, changes) = live.recordings();
    if changes == built {
        return;
    }
    let readers = match crate::rez_reader::RezReader::open_recordings(
        live.path(),
        std::sync::Arc::clone(&state.pool),
    ) {
        Ok(readers) => readers,
        Err(e) => {
            warn!(
                "{}: could not reread its recordings: {e}",
                live.path().display()
            );
            return;
        }
    };
    let before = state.members.read().len();
    let combined = super::combine_recordings(readers);
    warn!(
        "{} now holds {} recordings (was {before}): {}; its sections were rebuilt, reload \
         the page to see them",
        live.path().display(),
        labels.len(),
        labels
            .iter()
            .map(crate::mcp::render_labels)
            .collect::<Vec<_>>()
            .join("; ")
    );
    *state.members.write() = combined.members;
    state.captures.set_baseline_systeminfo(combined.systeminfo);
    if let Some(slot) = state.combined_live.lock().as_mut() {
        slot.1 = changes;
        // Only while this archive is still the baseline: an upload may have
        // replaced it during the reread.
        if Arc::ptr_eq(&slot.0, &live) {
            *state.baseline_file.write() = super::state::BaselineFile::Combined {
                recordings: combined.labels.len(),
            };
        }
    }
    regenerate_dashboards(state);
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
    let preference = state.template_preference;
    let baseline_path = state.parquet_path.read().clone();

    // An archive opened combined: one context over every recording, each
    // template once per recording that carries it. A comparison attached
    // over it later renders as the captures' path below does.
    let members = state.members.read().clone();
    if members.len() > 1 && state.captures.get(CaptureId::Experiment).is_none() {
        let filesize = baseline_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));
        let stem = baseline_path
            .as_deref()
            .and_then(|p| p.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| s.trim_end_matches(".parquet").to_string());
        let (context, notices) = ::dashboard::dashboard::build_combined_context(
            state.baseline_data().as_ref(),
            state.combined_catalog.read().clone(),
            &members,
            registry,
            preference,
            stem.as_deref(),
            filesize,
        );
        for notice in notices {
            warn_once(notice);
        }
        *state.sections.write() = LazySectionStore::new(context);
        return;
    }

    // Each capture's templates come from its own recording's metadata, and
    // each is validated against its own data so a KPI present only in one
    // recording isn't wrongly marked unavailable.
    let baseline_exts: Vec<(String, ServiceExtension)> = {
        let baseline_data = state.baseline_data();
        let mut exts = capture_service_extensions(baseline_data.as_ref(), registry, preference);
        validate_service_extensions(baseline_data.as_ref(), &mut exts);
        exts
    };
    let experiment_exts: Vec<(String, ServiceExtension)> = state
        .captures
        .get(CaptureId::Experiment)
        .map(|data| {
            let mut exts = capture_service_extensions(data.as_ref(), registry, preference);
            validate_service_extensions(data.as_ref(), &mut exts);
            exts
        })
        .unwrap_or_default();
    let mut service_exts = baseline_exts;
    service_exts.extend(experiment_exts);

    let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
    let category = lookup_category(state, registry, &service_refs);

    let filesize = baseline_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));

    let sources = classify_sources(
        baseline_path.as_deref(),
        state.baseline_data().as_ref(),
        &service_exts,
    );
    // The built-in sections render from the baseline's template: the
    // sections are generated against the baseline's data alone.
    let template = capture_dashboard_template(state.baseline_data().as_ref(), preference);
    let mut context = dashboard::dashboard::build_template_context(
        template,
        filesize,
        &service_refs,
        category,
        &sources,
    );
    // The recordings beside a comparison, each in its own sections.
    let beside = state.beside.read();
    if !beside.is_empty() {
        for notice in ::dashboard::dashboard::add_beside_sections(
            &mut context,
            state.baseline_data().as_ref(),
            &beside,
            registry,
            preference,
        ) {
            warn_once(notice);
        }
    }
    drop(beside);
    *state.sections.write() = LazySectionStore::new(context);
}

/// Classify all non-service sources present in the baseline parquet into
/// `SourceEntry` descriptors for the dashboard nav. Service sources are
/// excluded — they already appear via `service_exts`.
///
/// Single-source files are fully supported. Combined files make a best-effort
/// pass over `per_source_metadata` keys with the information available.
pub(super) fn classify_sources(
    baseline_path: Option<&std::path::Path>,
    baseline_data: &dyn metriken_query::MetricsSource,
    service_exts: &[(String, ServiceExtension)],
) -> Vec<dashboard::dashboard::SourceEntry> {
    // Nothing to classify without a file (live mode); its path gives the
    // filename stem a simple capture's section is named by.
    let path = match baseline_path {
        Some(p) => p,
        None => return vec![],
    };

    // The capture's metadata as a JSON object for the shared classifier: the
    // recording's own metadata. The WASM viewer builds the same object from
    // the same map.
    let mut kv_map = serde_json::Map::new();
    for (key, val) in baseline_data.file_metadata() {
        let json_val =
            serde_json::from_str(&val).unwrap_or_else(|_| serde_json::Value::String(val.clone()));
        kv_map.insert(key, json_val);
    }

    let service_names: std::collections::HashSet<&str> =
        service_exts.iter().map(|(s, _)| s.as_str()).collect();

    // Metric names for the self-sampler fingerprint.
    let mut metric_names: Vec<String> = baseline_data.counter_names();
    metric_names.extend(baseline_data.gauge_names());
    metric_names.extend(baseline_data.histogram_names());
    metric_names.extend(baseline_data.gauge_histogram_names());

    let filename_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        // strip .parquet.ab extension chains
        .map(|s| s.trim_end_matches(".parquet"));

    // Classification is shared with the WASM viewer (dashboard crate) so both
    // backends produce the same section list for a given recording.
    dashboard::source_kind::classify_sources(
        &serde_json::Value::Object(kv_map),
        &metric_names,
        &service_names,
        filename_stem,
    )
}

#[cfg(test)]
mod warn_once_tests {
    /// A notice is reported the first time and not after, so the terminal
    /// UI's per-tick regeneration does not repeat a warning every second.
    #[test]
    fn a_notice_is_new_only_once() {
        let notice = "warn_once_tests: a notice no other test raises";
        assert!(super::first_time(notice));
        assert!(!super::first_time(notice));
        assert!(super::first_time("warn_once_tests: a different notice"));
    }

    /// A KPI with a `unit_system` the viewer does not know is warned about
    /// once, at load; the render that drops it is silent.
    #[test]
    fn an_unknown_unit_system_is_warned_once_at_load() {
        let metadata: std::collections::HashMap<String, String> = [
            ("source", "warn-once-tests-unit"),
            (
                "service_queries",
                r#"{"service_name": "warn-once-tests-unit", "kpis": [{"role": "load",
                    "title": "Depth", "query": "q", "type": "gauge", "unit_system": "furlongs"}]}"#,
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let exts = super::service_extensions_from_metadata(
            &metadata,
            &::dashboard::TemplateRegistry::empty(),
            super::TemplatePreference::Archive,
        );
        assert_eq!(exts.len(), 1, "the extension is kept");
        let notice = format!(
            "service template for \"warn-once-tests-unit\": {}; rendering it with the default `count` scaling",
            exts[0].1.unknown_unit_systems()[0]
        );
        assert!(notice.contains("furlongs"), "{notice}");
        assert!(
            !super::first_time(&notice),
            "the load did not warn {notice:?}"
        );
    }

    /// The lookup reports a malformed template through `first_time`, so the
    /// same notice is not warned again on the next regeneration.
    #[test]
    fn the_lookup_records_the_notices_it_warns() {
        let metadata: std::collections::HashMap<String, String> = [
            ("source", "warn-once-tests-svc"),
            ("service_queries", r#"{"service_name": 7}"#),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let registry = ::dashboard::TemplateRegistry::empty();
        let expected = ::dashboard::source_templates::resolve_source_templates(
            &metadata,
            &registry,
            super::TemplatePreference::Archive,
        )
        .notices;
        assert_eq!(expected.len(), 1, "{expected:?}");
        super::service_extensions_from_metadata(
            &metadata,
            &registry,
            super::TemplatePreference::Archive,
        );
        assert!(
            !super::first_time(&expected[0]),
            "the lookup did not record {:?}",
            expected[0]
        );
    }
}

#[cfg(test)]
mod report_mode_tests {
    use super::*;
    use ::dashboard::TemplateRegistry;
    use metriken_query::MemoryStore;
    use std::sync::Arc;

    #[test]
    fn regenerate_returns_empty_sections_for_trimmed_report() {
        let store =
            Arc::new(MemoryStore::builder().build()) as Arc<dyn metriken_query::MetricsSource>;
        let state = AppState::new(store, TemplateRegistry::empty());
        *state.trimmed_report_marker.write() = Some("trimmed".to_string());
        regenerate_dashboards(&state);
        let sections = state.sections.read();
        assert!(
            sections.is_empty(),
            "trimmed report should have no sections"
        );
    }
}

#[cfg(test)]
mod dashboard_template_tests {
    //! The server viewer renders the built-in sections from a section
    //! template: the recording's `dashboard`, else the embedded one.
    use super::*;
    use ::dashboard::dashboard::{build_dashboard_context, generate_section};
    use metriken_query::MemoryStore;
    use std::collections::BTreeMap;

    /// A Rezolus capture with a few per-CPU and memory series, and `extra`
    /// in its metadata.
    fn store(extra: &[(&str, String)]) -> Arc<dyn MetricsSource> {
        let store = MemoryStore::builder()
            .source("rezolus")
            .filename("fixture.dendro")
            .build();
        let ts: Vec<u64> = (0..5)
            .map(|i| 1_700_000_000_000_000_000 + i * 1_000_000_000)
            .collect();
        let labels = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        for id in ["0", "1"] {
            for state in ["user", "system"] {
                store
                    .insert_counter_series(
                        "cpu_usage",
                        labels(&[("id", id), ("state", state)]),
                        ts.clone(),
                        vec![0, 10, 20, 30, 40],
                        None,
                    )
                    .unwrap();
            }
        }
        store
            .insert_gauge_series("cpu_cores", labels(&[]), ts.clone(), vec![2; 5], None)
            .unwrap();
        store
            .insert_gauge_series(
                "memory_total",
                labels(&[]),
                ts.clone(),
                vec![1 << 30; 5],
                None,
            )
            .unwrap();
        for (k, v) in extra {
            store.set_metadata(*k, v.clone());
        }
        Arc::new(store)
    }

    fn state(data: Arc<dyn MetricsSource>, preference: TemplatePreference) -> AppState {
        let mut state = AppState::new(data, TemplateRegistry::empty());
        state.template_preference = preference;
        regenerate_dashboards(&state);
        state
    }

    /// The Rezolus template with `edit` applied to its CPU section.
    fn edited_cpu(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut doc: serde_json::Value =
            serde_json::from_str(&::dashboard::rezolus_template::json()).unwrap();
        let cpu = doc["sections"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["route"] == "/cpu")
            .unwrap();
        edit(cpu);
        doc.to_string()
    }

    fn renamed_cpu(name: &str) -> String {
        edited_cpu(|cpu| cpu["name"] = serde_json::Value::String(name.to_string()))
    }

    fn cpu_name(state: &AppState) -> String {
        let store = state.sections.read();
        store
            .sections()
            .iter()
            .find(|s| s.route == "/cpu")
            .unwrap()
            .name
            .clone()
    }

    /// With no `dashboard` in the metadata, the navigation and every built-in
    /// section are what the generators render for the same capture.
    #[test]
    fn a_recording_without_a_dashboard_renders_what_the_generators_render() {
        let data = store(&[]);
        let state = state(Arc::clone(&data), TemplatePreference::Archive);
        let want_ctx = build_dashboard_context(None, &[], None, &[]);
        let store = state.sections.read();
        assert!(store.context().template.is_some());
        assert_eq!(
            serde_json::to_string(store.sections()).unwrap(),
            serde_json::to_string(&want_ctx.sections).unwrap()
        );
        for section in &want_ctx.sections {
            let mut want = generate_section(data.as_ref(), &section.route, &want_ctx).unwrap();
            want.set_filename(data.filename_or_default());
            let got = store.generate(&section.route, data.as_ref()).unwrap();
            assert_eq!(
                got,
                serde_json::to_value(&want).unwrap(),
                "{}",
                section.route
            );
        }
    }

    /// The recording's template renders, unless the registry is preferred:
    /// the navigation's name for the section, and the section's own content.
    #[test]
    fn a_recordings_dashboard_renders_unless_the_registry_is_preferred() {
        let doc = edited_cpu(|cpu| {
            cpu["name"] = "Processors".into();
            cpu["groups"][0]["name"] = "Busy".into();
        });
        let data = store(&[("dashboard", doc)]);
        let archive = state(Arc::clone(&data), TemplatePreference::Archive);
        assert_eq!(cpu_name(&archive), "Processors");
        let section = archive
            .sections
            .read()
            .generate("/cpu", data.as_ref())
            .unwrap();
        assert_eq!(section["groups"][0]["name"], "Busy");

        let registry = state(Arc::clone(&data), TemplatePreference::Registry);
        assert_eq!(cpu_name(&registry), "CPU");
        let section = registry
            .sections
            .read()
            .generate("/cpu", data.as_ref())
            .unwrap();
        assert_eq!(section["groups"][0]["name"], "Utilization");
    }

    /// A `dashboard` that does not parse, or is over the size limit, falls
    /// back to the embedded template, and the reason is logged once.
    #[test]
    fn a_refused_dashboard_falls_back_with_a_notice() {
        let malformed = r#"{"schema": 1, "sections": "server shell test"}"#.to_string();
        let mut oversized = renamed_cpu("Processors");
        oversized.push_str(
            &" ".repeat(::dashboard::source_templates::MAX_DASHBOARD_BYTES + 1 - oversized.len()),
        );
        for doc in [malformed, oversized] {
            let metadata: HashMap<String, String> = [("dashboard".to_string(), doc.clone())]
                .into_iter()
                .collect();
            let notice = ::dashboard::source_templates::resolve_dashboard_template(
                &metadata,
                TemplatePreference::Archive,
            )
            .notice
            .expect("refused");
            let state = state(store(&[("dashboard", doc)]), TemplatePreference::Archive);
            assert_eq!(cpu_name(&state), "CPU");
            assert!(!first_time(&notice), "not logged: {notice}");
        }
    }

    /// A section the template cannot expand is an empty section carrying the
    /// reason, logged once; the other sections render.
    #[test]
    fn a_section_that_cannot_expand_is_empty_and_the_others_render() {
        let doc = edited_cpu(|cpu| {
            cpu["groups"] = serde_json::json!([{"name": "g", "id": "g", "subgroups": [{"plots": [
                {"kind": "gauge", "title": "t", "id": "p", "unit": "count", "query": "${throughput}"}
            ]}]}]);
        });
        let data = store(&[("dashboard", doc)]);
        let state = state(Arc::clone(&data), TemplatePreference::Archive);
        let store = state.sections.read();
        let cpu = store.generate("/cpu", data.as_ref()).unwrap();
        assert_eq!(cpu["groups"], serde_json::json!([]));
        let error = cpu["metadata"]["template_error"].as_str().unwrap();
        assert!(error.contains("throughput"), "{error}");
        assert!(!first_time(&format!(
            "section /cpu could not be rendered from its dashboard template: {error}"
        )));

        let want_ctx = build_dashboard_context(None, &[], None, &[]);
        let mut want = generate_section(data.as_ref(), "/memory", &want_ctx).unwrap();
        want.set_filename(data.filename_or_default());
        assert_eq!(
            store.generate("/memory", data.as_ref()).unwrap(),
            serde_json::to_value(&want).unwrap()
        );
    }
}
