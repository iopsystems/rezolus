use std::sync::Arc;

use metriken_query::{Bytes, QueryEngine, Tsdb};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Parse a JS-side capture id into an internal slot selector. Mirrors
/// the server-side CaptureId::parse but lives here to avoid pulling
/// the viewer crate dependency graph into the wasm module.
#[derive(Copy, Clone)]
enum Slot {
    Baseline,
    Experiment,
}

impl Slot {
    fn parse(capture: &str) -> Result<Self, JsValue> {
        match capture {
            "baseline" => Ok(Slot::Baseline),
            "experiment" => Ok(Slot::Experiment),
            other => Err(JsValue::from_str(&format!("unknown capture id: {other}"))),
        }
    }
}

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct Viewer {
    engine: QueryEngine<Arc<Tsdb>>,
    file_metadata: std::collections::HashMap<String, String>,
    dashboard_sections: std::collections::HashMap<String, String>,
    /// Display alias for this capture, when the JS caller supplied
    /// one (e.g. via an `alias=path` static-site URL param). None
    /// means the UI falls back to the capture id.
    alias: Option<String>,
}

#[derive(Serialize)]
struct MetadataResponse {
    status: String,
    data: MetadataData,
}

#[derive(Serialize)]
struct MetadataData {
    #[serde(rename = "minTime")]
    min_time: f64,
    #[serde(rename = "maxTime")]
    max_time: f64,
    #[serde(rename = "fileChecksum")]
    file_checksum: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
}

#[derive(Serialize)]
struct ViewerInfo {
    interval: f64,
    source: String,
    version: String,
    filename: String,
    #[serde(rename = "minTime")]
    min_time: f64,
    #[serde(rename = "maxTime")]
    max_time: f64,
    counter_names: Vec<String>,
    gauge_names: Vec<String>,
    histogram_names: Vec<String>,
}

#[wasm_bindgen]
impl Viewer {
    #[wasm_bindgen(constructor)]
    pub fn new(data: &[u8], filename: &str) -> Result<Viewer, JsValue> {
        let bytes = Bytes::from(data.to_vec());
        let mut tsdb = Tsdb::load_from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&format!("Failed to load parquet: {}", e)))?;
        tsdb.set_filename(filename.to_string());

        let file_metadata = tsdb.file_metadata().clone();
        let dashboard_sections = dashboard::dashboard::generate(&tsdb, None, &[], None, None);
        let engine = QueryEngine::new(Arc::new(tsdb));

        Ok(Viewer {
            engine,
            file_metadata,
            dashboard_sections,
            alias: None,
        })
    }

    /// Set or clear the display alias for this capture. Pass `None`
    /// (via JS passing `null`/`undefined`) to clear. Cheap — just a
    /// field assignment.
    pub fn set_alias(&mut self, alias: Option<String>) {
        self.alias = alias;
    }

    /// Returns JSON metadata compatible with /api/v1/metadata
    pub fn metadata(&self) -> String {
        let tsdb = self.engine.tsdb();
        let (min_time, max_time) = tsdb
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));

        serde_json::to_string(&MetadataResponse {
            status: "success".to_string(),
            data: MetadataData {
                min_time,
                max_time,
                file_checksum: String::new(),
                alias: self.alias.clone(),
            },
        })
        .unwrap()
    }

    /// Returns JSON with viewer info (interval, source, version, metric names)
    pub fn info(&self) -> String {
        let tsdb = self.engine.tsdb();
        let (min_time, max_time) = tsdb
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));

        serde_json::to_string(&ViewerInfo {
            interval: tsdb.interval(),
            source: tsdb.source().to_string(),
            version: tsdb.version().to_string(),
            filename: tsdb.filename().to_string(),
            min_time,
            max_time,
            counter_names: tsdb
                .counter_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            gauge_names: tsdb
                .gauge_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            histogram_names: tsdb
                .histogram_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .unwrap()
    }

    /// Returns systeminfo JSON from parquet file metadata.
    ///
    /// For multi-node combined files (>1 node in per_source_metadata), returns
    /// an object keyed by node name with each node's systeminfo.  For single-node
    /// files, returns the flat systeminfo string.
    pub fn systeminfo(&self) -> Option<String> {
        // Try multi-node first
        if let Some(psm_str) = self.file_metadata.get("per_source_metadata") {
            if let Ok(psm) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
            {
                if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
                    let mut nodes = serde_json::Map::new();
                    for (sub_key, entry) in rez_group {
                        let obj = match entry.as_object() {
                            Some(o) => o,
                            None => continue,
                        };
                        let sysinfo_val = match obj.get("systeminfo") {
                            Some(v) => v,
                            None => continue,
                        };
                        let node_name = obj.get("node").and_then(|v| v.as_str()).unwrap_or(sub_key);
                        nodes.insert(node_name.to_string(), sysinfo_val.clone());
                    }
                    if nodes.len() > 1 {
                        return serde_json::to_string(&serde_json::Value::Object(nodes)).ok();
                    }
                }
            }
        }
        // Fall back to flat systeminfo
        self.file_metadata.get("systeminfo").cloned()
    }

    /// Returns selection JSON from parquet file metadata, or null
    pub fn selection(&self) -> Option<String> {
        self.file_metadata.get("selection").cloned()
    }

    /// Returns all file-level metadata as a JSON object, mirroring the
    /// server's /file_metadata endpoint.  Values that are valid JSON are
    /// embedded as-is; everything else becomes a JSON string.
    ///
    /// Includes pre-computed `nodes`, `node_versions`, and
    /// `service_instances` fields so the frontend doesn't have to
    /// re-parse `per_source_metadata` itself.
    pub fn file_metadata_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for (key, val) in &self.file_metadata {
            let json_val = serde_json::from_str(val)
                .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
            map.insert(key.clone(), json_val);
        }
        enrich_with_multi_node_info(&mut map);
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".into())
    }

    /// Execute a PromQL range query. Returns JSON compatible with
    /// /api/v1/query_range response format.
    pub fn query_range(&self, query: &str, start: f64, end: f64, step: f64) -> String {
        match self.engine.query_range(query, start, end, step) {
            Ok(result) => {
                let json = serde_json::to_string(&result).unwrap_or_else(|e| {
                    format!(
                        r#"{{"status":"error","error":"serialization error: {}"}}"#,
                        e
                    )
                });
                format!(r#"{{"status":"success","data":{}}}"#, json)
            }
            Err(e) => {
                let msg = format!("{}", e).replace('"', "\\\"");
                format!(r#"{{"status":"error","error":"{}"}}"#, msg)
            }
        }
    }

    /// Execute a PromQL instant query.
    pub fn query(&self, query: &str, time: f64) -> String {
        match self.engine.query(query, Some(time)) {
            Ok(result) => {
                let json = serde_json::to_string(&result).unwrap_or_else(|e| {
                    format!(
                        r#"{{"status":"error","error":"serialization error: {}"}}"#,
                        e
                    )
                });
                format!(r#"{{"status":"success","data":{}}}"#, json)
            }
            Err(e) => {
                let msg = format!("{}", e).replace('"', "\\\"");
                format!(r#"{{"status":"error","error":"{}"}}"#, msg)
            }
        }
    }

    /// Accept a JSON array of templates, detect which service extensions
    /// match the loaded parquet file, and regenerate dashboards accordingly.
    /// The array may include category templates (`category: true`) — those
    /// don't have per-KPI `query` fields and would fail to deserialize as
    /// `ServiceExtension`. Filter them out here; compare-mode bridging
    /// uses `regenerate_combined` which re-parses the full JSON.
    pub fn init_templates(&mut self, templates_json: &str) -> Result<(), JsValue> {
        let templates = parse_service_templates(templates_json)?;
        let registry = dashboard::TemplateRegistry::from_templates(templates);

        let service_exts = self.detect_and_validate_service_exts(&registry);

        let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
            .iter()
            .map(|(name, ext)| (name.as_str(), ext))
            .collect();

        self.dashboard_sections = dashboard::dashboard::generate(
            self.engine.tsdb(),
            None,
            &service_refs,
            None, // single-capture: no category
            None,
        );
        Ok(())
    }

    /// Detect this Viewer's matching service extensions from the
    /// registry (using `per_source_metadata` first, falling back to
    /// `tsdb.source()`) and validate KPI availability against the
    /// Viewer's own tsdb. Returns the validated extensions, ready to
    /// pass to `dashboard::dashboard::generate`.
    ///
    /// Template selection is driven entirely by the parquet's source
    /// metadata. Category membership for compare-mode follows from the
    /// detected source names (e.g. a parquet whose `per_source_metadata`
    /// contains `vllm` is what makes a capture the vllm member of the
    /// inference-library category). The user-facing legend / display
    /// alias is plumbed separately via `Viewer::set_alias` and never
    /// influences which template a capture binds to.
    fn detect_and_validate_service_exts(
        &self,
        registry: &dashboard::TemplateRegistry,
    ) -> Vec<(String, dashboard::ServiceExtension)> {
        let mut service_exts: Vec<(String, dashboard::ServiceExtension)> = Vec::new();

        if let Some(psm_str) = self.file_metadata.get("per_source_metadata") {
            if let Ok(psm) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
            {
                for (source_type, _group) in &psm {
                    if source_type == "rezolus" {
                        continue;
                    }
                    if let Some(ext) = registry.get(source_type) {
                        service_exts.push((source_type.clone(), ext.clone()));
                    }
                }
            }
        }
        if service_exts.is_empty() {
            let tsdb = self.engine.tsdb();
            let source = tsdb.source().to_string();
            if let Some(ext) = registry.get(&source) {
                service_exts.push((source, ext.clone()));
            }
        }

        // Validate against this capture's own tsdb so per-capture
        // unavailability is correctly reported.
        let tsdb = self.engine.tsdb();
        validate_service_extensions_inline(tsdb, &mut service_exts);
        service_exts
    }

    /// Returns the sections list as a JSON array.
    pub fn get_sections(&self) -> String {
        if let Some(json) = self.dashboard_sections.values().next() {
            if let Ok(view) = serde_json::from_str::<serde_json::Value>(json) {
                if let Some(sections) = view.get("sections") {
                    return sections.to_string();
                }
            }
        }
        "[]".to_string()
    }

    /// Returns the full View JSON for a dashboard section. The shared
    /// `sections` navigation array is stripped on the way out — callers
    /// fetch it once via `get_sections()`.
    pub fn get_section(&self, key: &str) -> Option<String> {
        let raw = self
            .dashboard_sections
            .get(&format!("{key}.json"))
            .or_else(|| self.dashboard_sections.get(key))?;
        let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
        strip_sections_from_section_body(&mut value);
        serde_json::to_string(&value).ok()
    }
}

/// Registry wrapping up to two `Viewer` instances keyed by capture id
/// ("baseline" / "experiment").  Mirrors the server-side `CaptureRegistry`
/// shape so the JS transport layer can address either capture uniformly.
///
/// This type is additive — existing single-capture `Viewer` consumers are
/// unaffected.
#[wasm_bindgen]
pub struct WasmCaptureRegistry {
    baseline: Option<Viewer>,
    experiment: Option<Viewer>,
}

#[wasm_bindgen]
impl WasmCaptureRegistry {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            baseline: None,
            experiment: None,
        }
    }

    /// Attach a parquet capture under the given slot ("baseline" or
    /// "experiment").  Replaces any previously attached capture in that slot.
    pub fn attach(&mut self, capture: &str, data: &[u8], filename: &str) -> Result<(), JsValue> {
        let viewer = Viewer::new(data, filename)?;
        *self.slot_mut(Slot::parse(capture)?) = Some(viewer);
        Ok(())
    }

    /// Set or clear the display alias for a capture slot. No-op when
    /// the slot is empty.
    pub fn set_alias(&mut self, capture: &str, alias: Option<String>) -> Result<(), JsValue> {
        if let Some(viewer) = self.slot_mut(Slot::parse(capture)?).as_mut() {
            viewer.set_alias(alias);
        }
        Ok(())
    }

    /// Drop the capture in the given slot (no-op if unknown or empty).
    pub fn detach(&mut self, capture: &str) {
        if let Ok(slot) = Slot::parse(capture) {
            *self.slot_mut(slot) = None;
        }
    }

    /// Whether a capture is currently attached in the given slot.
    pub fn has(&self, capture: &str) -> bool {
        self.slot(capture).is_some()
    }

    pub fn metadata(&self, capture: &str) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.metadata())
    }

    pub fn info(&self, capture: &str) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.info())
    }

    pub fn systeminfo(&self, capture: &str) -> Option<String> {
        self.slot(capture).and_then(|v| v.systeminfo())
    }

    pub fn selection(&self, capture: &str) -> Option<String> {
        self.slot(capture).and_then(|v| v.selection())
    }

    pub fn file_metadata_json(&self, capture: &str) -> Option<String> {
        self.slot(capture).map(|v| v.file_metadata_json())
    }

    pub fn query_range(
        &self,
        capture: &str,
        query: &str,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<String, JsValue> {
        self.require_slot(capture)
            .map(|v| v.query_range(query, start, end, step))
    }

    pub fn query(&self, capture: &str, query: &str, time: f64) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.query(query, time))
    }

    /// Initialise ServiceExtension templates for the given capture.  Mirrors
    /// `Viewer::init_templates`.
    pub fn init_templates(&mut self, capture: &str, templates_json: &str) -> Result<(), JsValue> {
        let slot = Slot::parse(capture)?;
        self.slot_mut(slot)
            .as_mut()
            .ok_or_else(|| JsValue::from_str("capture not attached"))?
            .init_templates(templates_json)
    }

    /// Regenerate BOTH viewers' `dashboard_sections` using service
    /// extensions from BOTH attached captures and the explicitly named
    /// category template (when provided). When the experiment slot is
    /// empty, this is a no-op (the per-capture `init_templates` call
    /// already populated baseline's sections).
    ///
    /// Both slots get the same combined map: compare-mode chart fetches
    /// query both slots for the active section route, so a category
    /// route like `/service/inference-library` must resolve in the
    /// experiment slot too — otherwise the experiment fetch 404s and
    /// the chart surfaces "Error: null".
    ///
    /// `category_name` activates category mode when each detected
    /// source appears in the category template's `members` list. When
    /// the membership check fails (or the category template isn't
    /// found), category mode is silently skipped and the captures
    /// render as per-member sections — same fall-back shape the server
    /// runtime uses. A None category is treated as plain per-member
    /// compare mode (no bridging).
    ///
    /// Display aliases for the captures (the user-facing legend) are
    /// plumbed separately via `Viewer::set_alias` and never affect
    /// template lookup or category membership; that is determined
    /// entirely by each capture's parquet source metadata.
    pub fn regenerate_combined(
        &mut self,
        templates_json: &str,
        category_name: Option<String>,
    ) -> Result<(), JsValue> {
        // Both captures must be attached; otherwise nothing to combine.
        if self.experiment.is_none() || self.baseline.is_none() {
            return Ok(());
        }

        let templates = parse_service_templates(templates_json)?;
        // Reconstruct registry — same shape used by the per-capture
        // `init_templates`. The JSON may include both service templates
        // and category templates; the loader routes them by `category: true`.
        let registry = parse_template_registry(templates_json, &templates)?;

        // Each capture detects its own service extensions, validates
        // against its own tsdb (so a KPI present only in the experiment
        // doesn't get marked unavailable by the baseline tsdb).
        let baseline_exts = self
            .baseline
            .as_ref()
            .map(|v| v.detect_and_validate_service_exts(&registry))
            .unwrap_or_default();
        let experiment_exts = self
            .experiment
            .as_ref()
            .map(|v| v.detect_and_validate_service_exts(&registry))
            .unwrap_or_default();

        let mut service_exts: Vec<(String, dashboard::ServiceExtension)> = Vec::new();
        service_exts.extend(baseline_exts);
        service_exts.extend(experiment_exts);

        let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
            .iter()
            .map(|(name, ext)| (name.as_str(), ext))
            .collect();

        // Fall back to per-member rendering when the requested category
        // doesn't activate cleanly — same shape as the server runtime's
        // `lookup_category`. The user perceives this as "two per-member
        // sections instead of one combined section," which is a less
        // surprising failure mode than a hard error from the bootstrap.
        let category = match category_name.as_deref() {
            Some(name) => match registry.get_category(name) {
                Some(cat)
                    if service_refs.len() == 2
                        && service_refs
                            .iter()
                            .all(|(source, _)| cat.members.iter().any(|m| m == source)) =>
                {
                    Some((cat.service_name.as_str(), cat))
                }
                _ => None,
            },
            None => None,
        };

        let combined = dashboard::dashboard::generate(
            self.baseline.as_ref().unwrap().engine.tsdb(),
            None,
            &service_refs,
            category,
            None,
        );
        if let Some(baseline) = self.baseline.as_mut() {
            baseline.dashboard_sections = combined.clone();
        }
        if let Some(experiment) = self.experiment.as_mut() {
            experiment.dashboard_sections = combined;
        }
        Ok(())
    }

    pub fn get_sections(&self, capture: &str) -> Option<String> {
        self.slot(capture).map(|v| v.get_sections())
    }

    pub fn get_section(&self, capture: &str, section: &str) -> Option<String> {
        self.slot(capture).and_then(|v| v.get_section(section))
    }

    fn slot(&self, capture: &str) -> Option<&Viewer> {
        match Slot::parse(capture).ok()? {
            Slot::Baseline => self.baseline.as_ref(),
            Slot::Experiment => self.experiment.as_ref(),
        }
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut Option<Viewer> {
        match slot {
            Slot::Baseline => &mut self.baseline,
            Slot::Experiment => &mut self.experiment,
        }
    }

    fn require_slot(&self, capture: &str) -> Result<&Viewer, JsValue> {
        Slot::parse(capture)?;
        self.slot(capture)
            .ok_or_else(|| JsValue::from_str("capture not attached"))
    }
}

impl Default for WasmCaptureRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop the shared `sections` navigation array from a generated section
/// body. The full nav list is exposed separately via `Viewer::get_sections`,
/// so per-section payloads don't need to carry it.
fn strip_sections_from_section_body(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sections");
    }
}

/// Parse a templates JSON array into the service-extension subset,
/// silently skipping `category: true` entries (those have a different
/// schema and are handled separately by `parse_template_registry`).
fn parse_service_templates(
    templates_json: &str,
) -> Result<Vec<dashboard::ServiceExtension>, JsValue> {
    let parsed: Vec<serde_json::Value> = serde_json::from_str(templates_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse templates: {}", e)))?;
    let mut templates = Vec::new();
    for v in parsed {
        if v.get("category").and_then(|b| b.as_bool()).unwrap_or(false) {
            continue;
        }
        let ext: dashboard::ServiceExtension = serde_json::from_value(v)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse template: {}", e)))?;
        templates.push(ext);
    }
    Ok(templates)
}

/// Build a TemplateRegistry from a list of service extensions PLUS
/// any category entries embedded in the same JSON. The frontend ships
/// both kinds in one templates list; the per-capture init_templates
/// path discards categories, so we re-parse here to recover them.
fn parse_template_registry(
    templates_json: &str,
    services: &[dashboard::ServiceExtension],
) -> Result<dashboard::TemplateRegistry, JsValue> {
    // Round-trip via TemplateRegistry::from_templates for the
    // services, then patch in categories by manually parsing the JSON
    // for entries with `category: true`. This avoids exposing a
    // category-aware constructor on TemplateRegistry that doesn't yet
    // exist for WASM.
    let mut registry = dashboard::TemplateRegistry::from_templates(services.to_vec());

    let parsed: Vec<serde_json::Value> = serde_json::from_str(templates_json)
        .map_err(|e| JsValue::from_str(&format!("re-parse templates: {e}")))?;
    for v in parsed {
        if v.get("category").and_then(|b| b.as_bool()).unwrap_or(false) {
            let category: dashboard::CategoryExtension = serde_json::from_value(v)
                .map_err(|e| JsValue::from_str(&format!("Failed to parse category: {e}")))?;
            registry.insert_category(category);
        }
    }
    Ok(registry)
}

/// Validate KPI availability for service extensions by running each KPI's
/// PromQL query against `tsdb`. Sets `available = false` on KPIs whose
/// queries return empty results. WASM-targeted mirror of the server's
/// `validate_service_extensions` in `src/viewer/mod.rs`.
fn validate_service_extensions_inline(
    tsdb: &Tsdb,
    exts: &mut [(String, dashboard::ServiceExtension)],
) {
    use metriken_query::promql;
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

/// Enrich a file-metadata JSON map with pre-computed multi-node info.
///
/// Parses `per_source_metadata` and adds `nodes`, `node_versions`, and
/// `service_instances` so the frontend doesn't have to duplicate this logic.
fn enrich_with_multi_node_info(map: &mut serde_json::Map<String, serde_json::Value>) {
    let psm = match map.get("per_source_metadata").and_then(|v| v.as_object()) {
        Some(psm) => psm.clone(),
        None => return,
    };

    let mut nodes = Vec::new();
    let mut node_versions = serde_json::Map::new();
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (sub_key, entry) in rez_group {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
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
        let group_obj = match group.as_object() {
            Some(o) => o,
            None => continue,
        };
        let mut instances = Vec::new();
        for (sub_key, entry) in group_obj {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_sections_from_generated_section_body() {
        let mut value = serde_json::json!({
            "sections": [{"name": "Overview", "route": "/overview"}],
            "groups": []
        });
        strip_sections_from_section_body(&mut value);
        assert!(value.get("sections").is_none());
        assert_eq!(value["groups"], serde_json::json!([]));
    }
}
