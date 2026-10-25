use std::sync::Arc;

use bytes::Bytes;
use metriken_query::{MetricsSource, QueryOptions};
use metriken_storage::BufferPool;

/// Buffer pool budget for the WASM viewer: 64 MB.
///
/// Browsers impose tighter heap limits than servers; the source bytes
/// are already in memory, so the effective warm-cache speedup is bounded
/// by how many row groups fit alongside them. 64 MB is a conservative
/// default — enough for a moderate recording without crowding the JS heap.
const WASM_CACHE_SIZE_BYTES: usize = 64 * 1024 * 1024;
use dashboard::keys::{KEY_DESCRIPTIONS, KEY_PER_SOURCE_METADATA, KEY_SYSTEMINFO};
use dashboard::source_templates::TemplatePreference;
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// The template preference a `set_prefer_registry_templates(bool)` names.
fn preference(prefer_registry: bool) -> TemplatePreference {
    if prefer_registry {
        TemplatePreference::Registry
    } else {
        TemplatePreference::Archive
    }
}

/// The anchor capture's id — what an absent `?capture=` resolves to, never
/// renamed. Mirrors the server registry's `BASELINE_ID`.
const BASELINE_ID: &str = "baseline";

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Mirrors the server's `regenerate_dashboards` short-circuit: a parquet
/// produced by Save as Report carries `KEY_REPORT = "trimmed"` in its
/// footer, and the viewer should suppress the rezolus / service / Query
/// Explorer sections (their columns were trimmed away). The frontend
/// reads the same marker out of `getFileMetadata()` and routes to
/// `/report` instead of the default landing.
fn is_trimmed_report(file_metadata: &std::collections::HashMap<String, String>) -> bool {
    file_metadata.get("report").map(String::as_str) == Some("trimmed")
}

/// `DashboardContext::default()` returns sections=[], filesize=None — the
/// shape callers want when a parquet should render only the Report view.
fn empty_dashboard_context() -> dashboard::dashboard::DashboardContext {
    dashboard::dashboard::DashboardContext::default()
}

/// The section template a capture's built-in sections render from, and why
/// the one its metadata carries was refused, if it was. The same choice as the
/// server viewer's `metadata::capture_dashboard_template`.
fn dashboard_template(
    file_metadata: &std::collections::HashMap<String, String>,
    preference: TemplatePreference,
) -> (
    Arc<dashboard::section_template::SectionTemplate>,
    Option<String>,
) {
    let chosen = dashboard::source_templates::resolve_dashboard_template(file_metadata, preference);
    (chosen.template, chosen.notice)
}

/// Classify the sources in a loaded parquet into the `SourceEntry` list the
/// dashboard renderer consumes. Delegates to `dashboard::source_kind` — the
/// SAME classifier the axum server calls in `src/viewer/metadata.rs` — so both
/// backends produce identical section lists (a simple-capture parquet gets its
/// `source:` nav entry in the WASM viewer, not just the server). `service_names`
/// are the sources already covered by a service template (excluded here).
fn classify_sources(
    reader: &dyn MetricsSource,
    file_metadata: &std::collections::HashMap<String, String>,
    service_names: &std::collections::HashSet<&str>,
) -> Vec<dashboard::dashboard::SourceEntry> {
    // Reassemble file-level metadata as a JSON object — same shape the server
    // reads from the parquet footer (valid JSON embeds as-is, else a string).
    let mut map = serde_json::Map::new();
    for (key, val) in file_metadata {
        let json_val =
            serde_json::from_str(val).unwrap_or_else(|_| serde_json::Value::String(val.clone()));
        map.insert(key.clone(), json_val);
    }

    let mut metric_names: Vec<String> = reader.counter_names();
    metric_names.extend(reader.gauge_names());
    metric_names.extend(reader.histogram_names());
    metric_names.extend(reader.gauge_histogram_names());

    let filename = reader.filename_or_default();
    let filename_stem = filename
        .strip_suffix(".parquet")
        .or_else(|| filename.strip_suffix(".rez"))
        .unwrap_or(&filename);

    dashboard::source_kind::classify_sources(
        &serde_json::Value::Object(map),
        &metric_names,
        service_names,
        Some(filename_stem),
    )
}

/// The refusal of an upload that is not a dendro archive. The browser
/// cannot convert one: a recording an earlier release wrote is converted on
/// the user's machine, with the command named here, and the `.dendro`
/// opened instead.
fn not_dendro(filename: &str, container: rez::container::Container) -> String {
    if container.is_legacy() {
        format!(
            "{filename} is a {}, which this viewer does not open. Convert it to a .dendro \
             on your machine, then open the .dendro:\n  rezolus recording upgrade {}",
            container.name(),
            rez::container::shell_word(filename)
        )
    } else {
        format!("{filename} is not a recording (a .dendro archive)")
    }
}

/// Open every recording of a dendro archive held as bytes.
///
/// The one place the browser turns an uploaded archive into readers, so both
/// entry points — a single capture and the two-slot registry — agree on what
/// the file contains. Any other file is refused ([`not_dendro`]).
fn open_rez(
    data: &[u8],
    filename: &str,
    pool: Arc<BufferPool>,
) -> Result<rez::reader::LabeledRecordings, JsValue> {
    let container = rez::container::Container::of_bytes(data);
    if container != rez::container::Container::Dendro {
        return Err(JsValue::from_str(&not_dendro(filename, container)));
    }
    let recordings = rez::reader::RezReader::open_recordings_from_bytes(data.to_vec(), pool)
        .map_err(|e| JsValue::from_str(&format!("Failed to load {filename}: {e}")))?;
    if recordings.is_empty() {
        return Err(JsValue::from_str(&format!(
            "{filename} holds no recordings"
        )));
    }
    Ok(recordings)
}

/// Whether an archive's recordings open as one combined view: more than one
/// recording, and `dashboard::combined::layout` says combined. The server
/// viewer's `opens_combined`.
fn opens_combined(recordings: &rez::reader::LabeledRecordings, compare_requested: bool) -> bool {
    let labels: Vec<std::collections::BTreeMap<String, String>> =
        recordings.iter().map(|(l, _)| l.clone()).collect();
    recordings.len() > 1
        && dashboard::combined::layout(&labels, compare_requested)
            == dashboard::combined::Layout::Combined
}

/// Which recordings of a comparison fill its slots (the baseline arm, then
/// the experiment arm) and which are shown beside it, in the archive's
/// order (`dashboard::combined::arm_slots`, as the server viewer fills its
/// slots), and what to say about the choice. An error naming the
/// recordings when their arms cannot fill two slots.
type ComparisonOrder = (Vec<usize>, Vec<usize>, Option<String>);

fn comparison_order(
    labels: &[std::collections::BTreeMap<String, String>],
    filename: &str,
) -> Result<ComparisonOrder, String> {
    let slots = dashboard::combined::arm_slots(labels).map_err(|why| {
        let listing: Vec<String> = labels
            .iter()
            .map(|l| {
                l.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect();
        format!(
            "{filename}: {why}; it holds: {}. Open it with `rezolus view` and \
             --baseline/--experiment",
            listing.join("; ")
        )
    })?;
    let first: Vec<usize> = std::iter::once(slots.baseline)
        .chain(slots.experiment)
        .collect();
    let beside = (0..labels.len()).filter(|i| !first.contains(i)).collect();
    Ok((first, beside, slots.note))
}

/// What to say about an archive that was never cleanly finalized, or `None`
/// when every recording in it was.
///
/// **This is the one place the browser can hold less than the CLI would show
/// for the same recording, so it has to be said rather than logged.** A `.rez`
/// keeps its own unsealed rows in a `wal` TABLE inside the file, and those
/// travel fine. SQLite ALSO commits into a `-wal` sidecar — a separate file
/// holding pages not yet checkpointed into the archive. `rezolus view
/// archive.rez` opens by path and SQLite reads that sidecar; a browser is
/// handed one file and cannot. So a copy taken from under a running recorder
/// reads short here and complete there, by up to a checkpoint's worth of ticks.
///
/// The archive knows which case it is in: `complete` stays 0 until finalize.
/// `RezReader` already warned through `tracing`, which in wasm goes nowhere.
fn incomplete_notice(
    recordings: &rez::reader::LabeledRecordings,
    filename: &str,
) -> Option<String> {
    if recordings.iter().all(|(_, r)| r.complete()) {
        return None;
    }
    Some(format!(
        "{filename} was not cleanly finalized: it reads up to its last checkpoint, and any \
         ticks after that are not in this file. If it was copied while a recorder still held \
         it, the copy is also missing whatever was still in SQLite's `-wal` sidecar — a \
         separate file that does not travel with an upload. `rezolus recording snapshot \
         <archive> -o out.rez` takes a complete copy without stopping the recorder."
    ))
}

#[wasm_bindgen]
pub struct Viewer {
    /// The loaded capture. Type-erased because a capture is a parquet file or
    /// one recording of a `.rez` archive, and every method below asks it only
    /// for what `MetricsSource` already answers.
    reader: Arc<dyn MetricsSource>,
    /// Per-Viewer buffer pool. WASM is single-upload-per-Viewer; each
    /// Viewer gets its own pool sized at `WASM_CACHE_SIZE_BYTES`.
    _pool: Arc<BufferPool>,
    file_metadata: std::collections::HashMap<String, String>,
    /// Lazy section context. Populated by `init_templates` (single
    /// capture) or `WasmCaptureRegistry::regenerate_combined` (compare
    /// mode). When no templates are loaded, this is `Default` (empty
    /// nav), and `get_sections` returns `"[]"`.
    context: dashboard::dashboard::DashboardContext,
    /// Memoized rendered bodies keyed by route stem (e.g. `"cpu"`,
    /// `"service/vllm"`). RefCell because WASM is single-threaded and
    /// the wasm-bindgen surface keeps `get_section` as `&self` to avoid
    /// churning the JS-side method binding.
    cached_bodies: std::cell::RefCell<std::collections::HashMap<String, serde_json::Value>>,
    /// Display alias for this capture, when the JS caller supplied
    /// one (e.g. via an `alias=path` static-site URL param). None
    /// means the UI falls back to the capture id.
    alias: Option<String>,
    /// Original parquet bytes kept alongside the reader. Save as Report
    /// re-encodes a projection from this source, not from the reader
    /// (which has lost internal Arrow field metadata like the
    /// `metric` key used by `parquet filter`'s keep-field predicate).
    ///
    /// Empty for a capture that came out of a `.rez`: an archive is many
    /// parquet tables, not the single file the report projection re-encodes.
    /// [`Viewer::can_save`] is what callers ask, rather than testing this.
    source_bytes: Bytes,
    /// Whose KPI template renders when this capture's metadata carries one
    /// and the registry knows the source too. The server's
    /// `--registry-templates` flag; see `dashboard::source_templates`.
    template_preference: TemplatePreference,
    /// The recordings of an archive opened combined (`dashboard::combined`),
    /// which the sections are built from
    /// (`dashboard::dashboard::build_combined_context`). Empty otherwise.
    members: Vec<dashboard::combined::Member>,
    /// A combined archive's systeminfo, one recording's or each recording's
    /// by name (`dashboard::combined::assemble_members`); its metadata is
    /// the first recording's.
    combined_systeminfo: Option<String>,
    /// The recordings shown beside a comparison, in their own sections
    /// (`dashboard::dashboard::add_beside_sections`); `reader` is then a
    /// `dashboard::combined::Routed` over them. Empty otherwise.
    beside: Vec<dashboard::dashboard::Beside>,
    /// The labels of the archive recording this capture reads, when it is a
    /// comparison's baseline: a report is saved onto it
    /// (`report_save::build_report`'s `anchor`).
    anchor_labels: Option<std::collections::BTreeMap<String, String>>,
    /// For a combined capture, the archive its reader joins, which answers
    /// each recording's catalog without opening other recordings' tables
    /// (`dashboard::combined::ScopeCatalog`).
    catalog: Option<Arc<dyn dashboard::combined::ScopeCatalog>>,
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
    // Native sampling interval (seconds). The frontend uses this to pick
    // the query step; without it, data.js falls back to a 1s step and a
    // coarse recording is over-queried. Mirror of the server viewer's
    // /api/v1/metadata `interval` field.
    interval: f64,
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
        let pool = BufferPool::new(WASM_CACHE_SIZE_BYTES);

        // The archive is recognized by CONTENT, not by the name the browser
        // gives us: a file dragged in may be called anything, and the server
        // viewer sniffs the container the same way.
        let mut recordings = open_rez(data, filename, Arc::clone(&pool))?;
        // An archive of one run is one capture: its recordings joined.
        if opens_combined(&recordings, false) {
            return Ok(Self::combined(recordings, pool));
        }
        if recordings.len() > 1 {
            // One `Viewer` is one capture. Silently taking the first
            // recording would show one arm of an A/B under the whole
            // archive's name — the failure this format's readers refuse
            // everywhere else. `WasmCaptureRegistry::attach` is the entry
            // point that can fill both slots.
            return Err(JsValue::from_str(&format!(
                "{filename} holds {} recordings of a comparison; open it as a comparison \
                 rather than a single capture",
                recordings.len()
            )));
        }
        let (_, reader) = recordings.remove(0);
        // The archive's bytes are kept: a report of a two-file comparison
        // is built from both captures' archives.
        Ok(Self::from_source(
            Arc::new(reader),
            pool,
            Bytes::copy_from_slice(data),
            filename,
        ))
    }

    /// One capture over every recording of an archive that opens
    /// combined: the server viewer's `init_file_mode_combined`.
    fn combined(recordings: rez::reader::LabeledRecordings, pool: Arc<BufferPool>) -> Viewer {
        let parts: Vec<_> = recordings
            .iter()
            .map(|(labels, reader)| {
                (
                    labels.clone(),
                    dashboard::combined::Header::of(reader),
                    reader.file_metadata(),
                )
            })
            .collect();
        // A report's selection, marker and events from whichever recording
        // carries the selection: one saved from a comparison of this archive
        // sits on its baseline, not necessarily the first recording.
        let markers = dashboard::combined::report_markers(
            &recordings
                .iter()
                .map(|(_, r)| r as &dyn MetricsSource)
                .collect::<Vec<_>>(),
        );
        let filename = recordings.first().and_then(|(_, r)| r.filename());
        let (reader, series) = rez::reader::RezReader::combine(recordings, filename);
        let (members, systeminfo) = dashboard::combined::assemble_members(
            parts
                .into_iter()
                .zip(series)
                .map(|((l, header, metadata), series)| (l, series, header, metadata))
                .collect(),
        );
        let reader = Arc::new(reader);
        let catalog: Arc<dyn dashboard::combined::ScopeCatalog> =
            Arc::new(dashboard::combined::FixedCatalog(Arc::clone(&reader)));
        let mut viewer = Self::from_source_bytes(reader, pool, Bytes::new());
        viewer.catalog = Some(catalog);
        viewer.members = members;
        viewer.combined_systeminfo = systeminfo;
        for (key, value) in markers {
            viewer.file_metadata.insert(key.to_string(), value);
        }
        if !is_trimmed_report(&viewer.file_metadata) {
            viewer.context = viewer
                .combined_context(&dashboard::TemplateRegistry::empty())
                .0;
        }
        viewer
    }

    /// The section context of a combined capture against `registry`, and
    /// the template lookups' notices.
    fn combined_context(
        &self,
        registry: &dashboard::TemplateRegistry,
    ) -> (dashboard::dashboard::DashboardContext, Vec<String>) {
        let filename = self.reader.filename_or_default();
        let stem = filename
            .strip_suffix(".parquet")
            .or_else(|| filename.strip_suffix(".rez"))
            .or_else(|| filename.strip_suffix(".dendro"))
            .unwrap_or(&filename);
        dashboard::dashboard::build_combined_context(
            self.reader.as_ref(),
            self.catalog.clone(),
            &self.members,
            registry,
            self.template_preference,
            Some(stem),
            None,
        )
    }

    /// Build a capture around an already-open source.
    ///
    /// `filename` is applied by the caller for a parquet (the reader carries
    /// it); a `.rez` recording names itself from its own labels, so this arm
    /// takes the archive's name only to report it.
    fn from_source(
        reader: Arc<dyn MetricsSource>,
        pool: Arc<BufferPool>,
        source_bytes: Bytes,
        _filename: &str,
    ) -> Viewer {
        Self::from_source_bytes(reader, pool, source_bytes)
    }

    fn from_source_bytes(
        reader: Arc<dyn MetricsSource>,
        pool: Arc<BufferPool>,
        source_bytes: Bytes,
    ) -> Viewer {
        let file_metadata = reader.file_metadata();
        let context = if is_trimmed_report(&file_metadata) {
            empty_dashboard_context()
        } else {
            // No templates loaded yet (init_templates refines this later); with
            // an empty service set a simple capture still classifies as its own
            // source, so its section is present even before templates arrive.
            let sources = classify_sources(
                reader.as_ref(),
                &file_metadata,
                &std::collections::HashSet::new(),
            );
            // The template's notice, if any, is returned by `init_templates`,
            // which rebuilds this context with the same choice.
            let (template, _) = dashboard_template(&file_metadata, TemplatePreference::Archive);
            dashboard::dashboard::build_template_context(template, None, &[], None, &sources)
        };

        Viewer {
            reader,
            _pool: pool,
            file_metadata,
            context,
            cached_bodies: std::cell::RefCell::new(std::collections::HashMap::new()),
            alias: None,
            source_bytes,
            template_preference: TemplatePreference::Archive,
            members: Vec::new(),
            combined_systeminfo: None,
            beside: Vec::new(),
            anchor_labels: None,
            catalog: None,
        }
    }

    /// Render KPI sections from the registry's template rather than the one
    /// this capture's metadata carries, for sources the registry knows. The
    /// server viewer's `--registry-templates`. Takes effect at the next
    /// `init_templates`.
    pub fn set_prefer_registry_templates(&mut self, prefer: bool) {
        self.template_preference = preference(prefer);
    }

    /// Whether Save as Report can build a report of this capture alone:
    /// true for a capture opened from its own one-recording archive, whose
    /// bytes it keeps. A capture that is one recording of a larger archive is
    /// saved through that archive (`WasmCaptureRegistry`) instead.
    pub fn can_save(&self) -> bool {
        !self.source_bytes.is_empty()
    }

    /// Set or clear the display alias for this capture. Pass `None`
    /// (via JS passing `null`/`undefined`) to clear. Cheap — just a
    /// field assignment.
    pub fn set_alias(&mut self, alias: Option<String>) {
        self.alias = alias;
    }

    /// Returns JSON metadata compatible with /api/v1/metadata
    pub fn metadata(&self) -> String {
        let (min_time, max_time) = self.reader.time_range().unwrap_or((0.0, 0.0));

        serde_json::to_string(&MetadataResponse {
            status: "success".to_string(),
            data: MetadataData {
                min_time,
                max_time,
                // Normalize a degenerate interval (0, or f64::MAX from an
                // empty multi-file reader) to 0.0 so the frontend's
                // `interval || 1` fallback engages. Mirrors routes.rs.
                interval: {
                    let i = self.reader.interval();
                    if i.is_finite() && i > 0.0 { i } else { 0.0 }
                },
                file_checksum: String::new(),
                alias: self.alias.clone(),
            },
        })
        .unwrap()
    }

    /// Returns JSON with viewer info (interval, source, version, metric names)
    pub fn info(&self) -> String {
        let (min_time, max_time) = self.reader.time_range().unwrap_or((0.0, 0.0));

        serde_json::to_string(&ViewerInfo {
            interval: self.reader.interval(),
            source: self.reader.source(),
            version: self.reader.version(),
            filename: self.reader.filename_or_default(),
            min_time,
            max_time,
            counter_names: self.reader.counter_names(),
            gauge_names: self.reader.gauge_names(),
            histogram_names: self.reader.histogram_names(),
        })
        .unwrap()
    }

    /// Returns the metric catalog JSON compatible with /api/v1/metrics.
    ///
    /// Mirrors the server-side handler: parses the `descriptions` map from
    /// file metadata, runs the catalog assembler, and wraps in MetricsResponse.
    pub fn metrics(&self, source: Option<String>) -> String {
        let resolved_source = source.clone().unwrap_or_else(|| self.reader.source());
        let meta = serde_json::json!({
            "descriptions": self.file_metadata.get(KEY_DESCRIPTIONS)
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
            "per_source_metadata": self.file_metadata.get(KEY_PER_SOURCE_METADATA)
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
        });
        let descriptions = dashboard::metric_catalog::resolve_descriptions(&meta, &resolved_source);
        let metrics = dashboard::metric_catalog::assemble_catalog(
            self.reader.as_ref(),
            &descriptions,
            source.as_deref(),
        );
        let response = dashboard::metric_catalog::MetricsResponse {
            source: resolved_source,
            metrics,
        };
        serde_json::to_string(&response).unwrap()
    }

    /// Returns JSON compatible with the server's /api/v1/timestamps: the raw,
    /// un-gridded per-sample collection timestamps (ns since epoch) for the
    /// jitter visualization. Mirrors `metrics()`'s source resolution.
    pub fn sample_timestamps(&self, source: Option<String>) -> String {
        let resolved_source = source.unwrap_or_else(|| self.reader.source());
        let timestamps = self.reader.sample_timestamps();
        serde_json::to_string(&serde_json::json!({
            "source": resolved_source,
            "timestamps": timestamps,
        }))
        .unwrap()
    }

    /// Returns systeminfo JSON from parquet file metadata.
    ///
    /// For multi-node combined files (>1 node in per_source_metadata), returns
    /// an object keyed by node name with each node's systeminfo.  For single-node
    /// files, returns the flat systeminfo string.
    /// This capture's systeminfo, with that of the recordings shown beside
    /// it (`dashboard::dashboard::with_beside_systeminfo`).
    pub fn systeminfo(&self) -> Option<String> {
        dashboard::dashboard::with_beside_systeminfo(
            self.own_systeminfo(),
            self.alias.as_deref().unwrap_or("baseline"),
            &self.beside,
        )
    }

    fn own_systeminfo(&self) -> Option<String> {
        // Try multi-node first
        if let Some(psm_str) = self.file_metadata.get(KEY_PER_SOURCE_METADATA)
            && let Ok(psm) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
            && let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object())
        {
            let mut nodes = serde_json::Map::new();
            for (sub_key, entry) in rez_group {
                let obj = match entry.as_object() {
                    Some(o) => o,
                    None => continue,
                };
                let sysinfo_val = match obj.get(KEY_SYSTEMINFO) {
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
        if !self.members.is_empty() {
            return self.combined_systeminfo.clone();
        }
        // Fall back to flat systeminfo
        self.file_metadata.get(KEY_SYSTEMINFO).cloned()
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
    ///
    /// Leaves out `dashboard`, the section template (about 106 KB), which
    /// the page has no use for; the server's endpoint leaves it out too.
    pub fn file_metadata_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for (key, val) in &self.file_metadata {
            if key == dashboard::source_templates::KEY_DASHBOARD {
                continue;
            }
            let json_val = serde_json::from_str(val)
                .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
            map.insert(key.clone(), json_val);
        }
        enrich_with_multi_node_info(&mut map);
        let own =
            serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".into());
        dashboard::dashboard::with_beside_metadata(&own, &self.beside)
    }

    /// Execute a PromQL range query. Returns JSON compatible with
    /// /api/v1/query_range response format. `rate_mode` is `"raw"` or (default)
    /// grid — see `dashboard::display_wire::parse_rate_mode`.
    pub fn query_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
        step: f64,
        rate_mode: &str,
    ) -> String {
        let qopts =
            QueryOptions::with_rate_mode(dashboard::display_wire::parse_rate_mode(Some(rate_mode)));
        match self
            .reader
            .query_range_opts(query, start, end, step, &qopts)
        {
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

    /// Display-mode range query. Returns the compact binary body (the server's
    /// layout — both go through `dashboard::display_wire`; an aggregate's values
    /// can differ in the last bits) so the shared frontend decodes it the same
    /// way. A non-series result (scalar/vector) is
    /// surfaced as an error so the frontend falls back to the JSON query path,
    /// matching the server. `band` is `"lo,hi"` (empty → interquartile default).
    #[allow(clippy::too_many_arguments)]
    pub fn query_range_display(
        &self,
        query: &str,
        start: f64,
        end: f64,
        step: f64,
        points: usize,
        band: &str,
        rate_mode: &str,
    ) -> Result<Vec<u8>, JsValue> {
        let band = dashboard::display_wire::parse_band(Some(band));
        let rate_mode = dashboard::display_wire::parse_rate_mode(Some(rate_mode));
        match dashboard::display_wire::display_query(
            self.reader.as_ref(),
            query,
            start,
            end,
            step,
            points,
            band,
            rate_mode,
        ) {
            Ok(dashboard::display_wire::DisplayWire::Binary(buf)) => Ok(buf),
            Ok(dashboard::display_wire::DisplayWire::Json(_)) => Err(JsValue::from_str(
                "display query returned a non-series result",
            )),
            Err(e) => Err(JsValue::from_str(&e.to_string())),
        }
    }

    /// Execute a PromQL instant query.
    pub fn query(&self, query: &str, time: f64) -> String {
        match self.reader.query(query, Some(time)) {
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
    ///
    /// Returns what the template lookup had to say about this capture, as a
    /// JSON array of strings: a template in the metadata that did not parse,
    /// and what rendered instead. The server viewer logs the same lines as
    /// warnings.
    pub fn init_templates(&mut self, templates_json: &str) -> Result<String, JsValue> {
        let templates = parse_service_templates(templates_json)?;
        let registry = dashboard::TemplateRegistry::from_templates(templates);

        if !self.members.is_empty() && !is_trimmed_report(&self.file_metadata) {
            let (context, notices) = self.combined_context(&registry);
            self.context = context;
            self.cached_bodies.borrow_mut().clear();
            return Ok(serde_json::to_string(&notices).unwrap_or_else(|_| "[]".to_string()));
        }

        let (service_exts, mut notices) =
            self.detect_and_validate_service_exts(&registry, self.template_preference);

        let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
            .iter()
            .map(|(name, ext)| (name.as_str(), ext))
            .collect();
        let service_names: std::collections::HashSet<&str> =
            service_exts.iter().map(|(name, _)| name.as_str()).collect();

        let context = if is_trimmed_report(&self.file_metadata) {
            empty_dashboard_context()
        } else {
            let sources =
                classify_sources(self.reader.as_ref(), &self.file_metadata, &service_names);
            let (template, notice) =
                dashboard_template(&self.file_metadata, self.template_preference);
            notices.extend(notice);
            let mut context = dashboard::dashboard::build_template_context(
                template,
                None,
                &service_refs,
                None, // single-capture: no category
                &sources,
            );
            notices.extend(dashboard::dashboard::add_beside_sections(
                &mut context,
                self.reader.as_ref(),
                &self.beside,
                &registry,
                self.template_preference,
            ));
            context
        };
        self.context = context;
        self.cached_bodies.borrow_mut().clear();
        Ok(serde_json::to_string(&notices).unwrap_or_else(|_| "[]".to_string()))
    }

    /// This Viewer's service extensions, validated against its own data.
    ///
    /// The capture's metadata decides: a `service_queries` template it
    /// carries (a parquet footer, a `.rez` recording, a dendro source), then
    /// the registry by source name (`per_source_metadata` keys first, then
    /// the `source` key), through the lookup the server viewer uses (`dashboard::source_templates::resolve_source_templates`).
    /// Returns the extensions and the lookup's notices.
    ///
    /// Category membership for compare-mode follows from the detected source
    /// names (e.g. a parquet whose `per_source_metadata` contains `vllm` is
    /// what makes a capture the vllm member of the inference-library
    /// category). The user-facing legend / display alias is plumbed
    /// separately via `Viewer::set_alias` and never influences which template
    /// a capture binds to.
    fn detect_and_validate_service_exts(
        &self,
        registry: &dashboard::TemplateRegistry,
        preference: TemplatePreference,
    ) -> (Vec<(String, dashboard::ServiceExtension)>, Vec<String>) {
        let resolved = dashboard::source_templates::resolve_source_templates(
            &self.file_metadata,
            registry,
            preference,
        );
        let mut service_exts = resolved.service_exts;

        // Validate against this capture's own reader so per-capture
        // unavailability is correctly reported.
        validate_service_extensions_inline(self.reader.as_ref(), &mut service_exts);
        (service_exts, resolved.notices)
    }

    /// Returns the sections list as a JSON array.
    pub fn get_sections(&self) -> String {
        serde_json::to_string(&self.context.sections).unwrap_or_else(|_| "[]".to_string())
    }

    /// Returns the full View JSON for a dashboard section. The shared
    /// `sections` navigation array is stripped on the way out — callers
    /// fetch it once via `get_sections()`.
    pub fn get_section(&self, key: &str) -> Option<String> {
        // Frontend may pass `"cpu"` or `"cpu.json"` — normalize to the
        // bare route stem the cache uses.
        let stem = key.strip_suffix(".json").unwrap_or(key);
        let route = format!("/{stem}");

        // Cache hit?
        if let Some(value) = self.cached_bodies.borrow().get(stem).cloned() {
            return serialize_lean_section(value);
        }

        // Render on demand. A section the template could not expand is an
        // empty view whose metadata carries the reason under
        // `template_error`, which the frontend logs.
        let mut view =
            dashboard::dashboard::render_section(self.reader.as_ref(), &route, &self.context)?.view;
        view.set_filename(self.reader.filename_or_default());
        if let Some(size) = self.context.filesize {
            view.set_filesize(size);
        }
        let mut value = serde_json::to_value(&view).ok()?;

        // Cache the FULL value (with sections) so a future re-render of
        // the same route can serve from cache without re-calling
        // generate_section. Strip on the way out so the frontend never
        // sees the embedded sections array.
        self.cached_bodies
            .borrow_mut()
            .insert(stem.to_string(), value.clone());
        strip_sections_from_section_body(&mut value);
        serde_json::to_string(&value).ok()
    }
}

fn serialize_lean_section(mut value: serde_json::Value) -> Option<String> {
    strip_sections_from_section_body(&mut value);
    serde_json::to_string(&value).ok()
}

/// The largest recording, in bytes, the browser saves a report of. A report
/// is built in memory: the upload, the archive converted from it and the
/// report's bytes, about three times the recording, against wasm32's 4 GiB
/// address space. Above it the save is refused ([`report_too_large`]).
pub const MAX_REPORT_SOURCE_BYTES: usize = 1 << 30;

/// The refusal for a Save as Report of a `size`-byte recording, when it is
/// over [`MAX_REPORT_SOURCE_BYTES`].
fn report_too_large(size: usize) -> Option<String> {
    (size > MAX_REPORT_SOURCE_BYTES).then(|| {
        format!(
            "this recording is {:.1} GiB; the browser saves a report of at most 1 GiB, \
             since it holds about three copies in memory. Save the report from `rezolus \
             view` (the server viewer reads the file from disk), or convert the file with \
             `rezolus recording upgrade` and shrink it with `rezolus recording filter` first",
            size as f64 / (1u64 << 30) as f64
        )
    })
}

/// Registry wrapping up to two `Viewer` instances keyed by capture id
/// ("baseline" / "experiment").  Mirrors the server-side `CaptureRegistry`
/// shape so the JS transport layer can address either capture uniformly.
///
/// This type is additive — existing single-capture `Viewer` consumers are
/// unaffected.
#[wasm_bindgen]
pub struct WasmCaptureRegistry {
    /// The anchor. Always addressed by id `baseline`.
    baseline: Option<Viewer>,
    /// Non-anchor captures in attach order, keyed by wire id; the first is
    /// conventionally `experiment`. The two-armed operations (A/B combine,
    /// report save) use `baseline` + this first entry — they stay pairwise.
    others: Vec<(String, Viewer)>,
    /// Things the UI should say out loud about the last attach — currently
    /// only "this archive holds more recordings than are being shown".
    ///
    /// The server viewer logs that to a terminal nobody is watching in a
    /// browser, so it is returned instead of dropped: an archive whose third
    /// arm is silently missing looks exactly like an archive with two arms.
    notices: Vec<String>,
    /// The uploaded `.rez` archive's own bytes, when the loaded dataset is a
    /// `.rez` (single or multi-recording). Retained because a Save-as-Report
    /// from a `.rez` source builds its report by trimming THIS archive — the
    /// per-capture `Viewer`s hold no source bytes for a `.rez`. `None` for a
    /// parquet dataset, where each capture carries its own parquet bytes.
    rez_source: Option<Bytes>,
    /// Applied to every capture's template lookup; see
    /// [`WasmCaptureRegistry::set_prefer_registry_templates`].
    template_preference: TemplatePreference,
    /// Whether a multi-recording archive attaches as a comparison whatever
    /// its recordings say; see [`WasmCaptureRegistry::set_compare_requested`].
    compare_requested: bool,
}

#[wasm_bindgen]
impl WasmCaptureRegistry {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            baseline: None,
            others: Vec::new(),
            notices: Vec::new(),
            rez_source: None,
            template_preference: TemplatePreference::Archive,
            compare_requested: false,
        }
    }

    /// Attach the next multi-recording archive as a comparison of its
    /// recordings even when they are of one run: the server viewer's
    /// `--baseline`/`--experiment`. Off by default, when such an archive
    /// attaches as one combined capture (`dashboard::combined::layout`).
    pub fn set_compare_requested(&mut self, requested: bool) {
        self.compare_requested = requested;
    }

    /// Attach a capture under the given slot ("baseline" or "experiment").
    /// Replaces any previously attached capture in that slot.
    ///
    /// A multi-recording archive attached as the baseline carries a whole
    /// comparison itself, so it attaches EVERY recording (under the shared
    /// id/alias scheme) and the requested slot id is ignored. That is the same
    /// mapping `rezolus view` makes for the same file. Attached as the
    /// experiment of a two-file comparison, an archive must be one recording;
    /// several are refused with the commands that compare two of them.
    pub fn attach(&mut self, capture: &str, data: &[u8], filename: &str) -> Result<(), JsValue> {
        self.notices.clear();
        let pool = BufferPool::new(WASM_CACHE_SIZE_BYTES);
        let recordings = open_rez(data, filename, Arc::clone(&pool))?;
        if capture != "baseline" && recordings.len() > 1 {
            let baseline = self
                .baseline
                .as_ref()
                .map(|b| b.reader.filename_or_default())
                .unwrap_or_else(|| "BASELINE.dendro".to_string());
            return Err(JsValue::from_str(&rez::container::one_recording_hint(
                "rezolus",
                filename,
                "the experiment",
                recordings.len(),
                &baseline,
                filename,
            )));
        }
        self.notices
            .extend(incomplete_notice(&recordings, filename));
        if recordings.len() >= 2 {
            // A report of this capture is a report of THIS archive.
            self.rez_source = Some(Bytes::copy_from_slice(data));
            if opens_combined(&recordings, self.compare_requested) {
                let viewer = Viewer::combined(recordings, pool);
                self.others.clear();
                self.set_capture(capture, viewer);
                return Ok(());
            }
            return self.attach_rez_comparison(recordings, pool, filename);
        }
        // One recording, a capture of its own: a report is built from its
        // archive, or from both captures' archives in a two-file comparison.
        if capture == "baseline" {
            self.rez_source = None;
        }
        let (_, reader) = recordings
            .into_iter()
            .next()
            .expect("open_rez rejects an empty archive");
        let viewer = Viewer::from_source(
            Arc::new(reader.with_filename(filename.to_string())),
            pool,
            Bytes::copy_from_slice(data),
            filename,
        );
        self.set_capture(capture, viewer);
        Ok(())
    }

    /// Load a multi-recording `.rez` into the two A/B slots.
    ///
    /// Recordings 0 and 1, in manifest order, matching `rezolus view`. Aliases
    /// come from `dashboard::capture_alias`, the same rule the server viewer
    /// uses, so one archive is labelled identically wherever it is opened —
    /// and scoped to the two recordings SHOWN, since a label can fail to tell
    /// three apart while telling the chosen two apart exactly.
    fn attach_rez_comparison(
        &mut self,
        recordings: rez::reader::LabeledRecordings,
        pool: Arc<BufferPool>,
        filename: &str,
    ) -> Result<(), JsValue> {
        // Attach EVERY recording, each under the shared id/alias scheme — the
        // same one the server viewer uses, so an archive names its captures
        // identically wherever it is opened. The browser has no `--baseline`/
        // `--experiment` selectors: the arms fill the two slots as the server
        // viewer fills them (`dashboard::combined::arm_slots`), and the rest
        // are named-or-positional.
        let labels: Vec<std::collections::BTreeMap<String, String>> =
            recordings.iter().map(|(l, _)| l.clone()).collect();
        let (slots, beside_idx, note) =
            comparison_order(&labels, filename).map_err(|e| JsValue::from_str(&e))?;
        self.notices.extend(note);
        let mut picked: Vec<Option<_>> = recordings.into_iter().map(Some).collect();
        let beside_labels: Vec<_> = beside_idx.iter().map(|&i| labels[i].clone()).collect();
        let beside: Vec<dashboard::dashboard::Beside> = beside_idx
            .iter()
            .zip(dashboard::dashboard::beside_names(
                &beside_labels,
                &beside_idx,
            ))
            .map(|(&i, (key, name))| {
                let (_, reader) = picked[i].take().expect("each index once");
                dashboard::dashboard::Beside {
                    key,
                    name,
                    data: Arc::new(reader) as Arc<dyn MetricsSource>,
                }
            })
            .collect();
        if !beside.is_empty() {
            // The server viewer logs the same line.
            self.notices.push(format!(
                "beside the comparison, each in its own sections: {}",
                beside
                    .iter()
                    .map(|b| b.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let recordings: rez::reader::LabeledRecordings = slots
            .iter()
            .map(|&i| picked[i].take().expect("each index once"))
            .collect();
        let labels: Vec<std::collections::BTreeMap<String, String>> =
            recordings.iter().map(|(l, _)| l.clone()).collect();
        let identities = dashboard::capture_alias::assign_capture_identities(&labels, &labels);
        // A report's selection, marker and events: the baseline's, else
        // those of the first recording with a selection
        // (`dashboard::combined::report_markers`); a report saved before the
        // baseline was chosen by arm sits on the first.
        let carried = {
            let shown: Vec<&dyn MetricsSource> = recordings
                .iter()
                .map(|(_, r)| r as &dyn MetricsSource)
                .chain(beside.iter().map(|b| b.data.as_ref()))
                .collect();
            dashboard::combined::report_markers(&shown)
        };

        let mut beside = Some(beside);
        for ((labels, reader), identity) in recordings.into_iter().zip(identities) {
            let primary: Arc<dyn MetricsSource> = Arc::new(reader);
            // The baseline answers the beside recordings' queries too.
            let beside = if identity.id == BASELINE_ID {
                beside.take().unwrap_or_default()
            } else {
                Vec::new()
            };
            let source: Arc<dyn MetricsSource> = if beside.is_empty() {
                primary
            } else {
                Arc::new(dashboard::combined::Routed::new(
                    primary,
                    beside
                        .iter()
                        .map(|b| (b.key.clone(), Arc::clone(&b.data)))
                        .collect(),
                ))
            };
            let mut viewer = Viewer::from_source(source, Arc::clone(&pool), Bytes::new(), filename);
            viewer.beside = beside;
            if identity.id == BASELINE_ID {
                viewer.anchor_labels = Some(labels);
                for (key, value) in &carried {
                    viewer.file_metadata.insert(key.to_string(), value.clone());
                }
            }
            viewer.set_alias(Some(identity.alias));
            self.set_capture(&identity.id, viewer);
        }
        Ok(())
    }

    /// What the UI should say about the last `attach`, as a JSON array of
    /// strings. Empty when there is nothing to report.
    pub fn notices(&self) -> String {
        serde_json::to_string(&self.notices).unwrap_or_else(|_| "[]".to_string())
    }

    /// Set or clear the display alias for a capture slot. No-op when
    /// the slot is empty.
    pub fn set_alias(&mut self, capture: &str, alias: Option<String>) -> Result<(), JsValue> {
        if let Some(viewer) = self.slot_by_id_mut(capture) {
            viewer.set_alias(alias);
        }
        Ok(())
    }

    /// Drop the capture in the given slot (no-op if unknown or empty).
    pub fn detach(&mut self, capture: &str) {
        if capture == BASELINE_ID {
            self.baseline = None;
        } else {
            self.others.retain(|(id, _)| id != capture);
        }
    }

    /// Whether a capture is currently attached in the given slot.
    pub fn has(&self, capture: &str) -> bool {
        self.slot(capture).is_some()
    }

    /// The attached captures as JSON `[{ "id", "alias" }]`, anchor first, in
    /// display order. The frontend enumerates this to know what to overlay —
    /// the browser analogue of the server's `/api/v1/captures`.
    pub fn captures(&self) -> String {
        let mut list: Vec<serde_json::Value> = Vec::new();
        let entry = |id: &str, viewer: &Viewer| {
            serde_json::json!({
                "id": id,
                "alias": viewer.alias.clone().unwrap_or_else(|| id.to_string()),
            })
        };
        if let Some(b) = self.baseline.as_ref() {
            list.push(entry(BASELINE_ID, b));
        }
        for (id, viewer) in &self.others {
            list.push(entry(id, viewer));
        }
        serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn metadata(&self, capture: &str) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.metadata())
    }

    pub fn info(&self, capture: &str) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.info())
    }

    pub fn metrics(&self, capture: &str, source: Option<String>) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.metrics(source))
    }

    /// Passthrough for `Viewer::sample_timestamps` — mirrors `metrics()`.
    pub fn timestamps(&self, capture: &str, source: Option<String>) -> Result<String, JsValue> {
        self.require_slot(capture)
            .map(|v| v.sample_timestamps(source))
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
        rate_mode: &str,
    ) -> Result<String, JsValue> {
        self.require_slot(capture)
            .map(|v| v.query_range(query, start, end, step, rate_mode))
    }

    pub fn query(&self, capture: &str, query: &str, time: f64) -> Result<String, JsValue> {
        self.require_slot(capture).map(|v| v.query(query, time))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn query_range_display(
        &self,
        capture: &str,
        query: &str,
        start: f64,
        end: f64,
        step: f64,
        points: usize,
        band: &str,
        rate_mode: &str,
    ) -> Result<Vec<u8>, JsValue> {
        self.require_slot(capture)
            .and_then(|v| v.query_range_display(query, start, end, step, points, band, rate_mode))
    }

    /// Initialise ServiceExtension templates for the given capture.  Mirrors
    /// `Viewer::init_templates`, and returns that capture's template notices
    /// as a JSON array of strings.
    pub fn init_templates(
        &mut self,
        capture: &str,
        templates_json: &str,
    ) -> Result<String, JsValue> {
        let preference = self.template_preference;
        let viewer = self
            .slot_by_id_mut(capture)
            .ok_or_else(|| JsValue::from_str("capture not attached"))?;
        viewer.template_preference = preference;
        viewer.init_templates(templates_json)
    }

    /// Render KPI sections from the registry's template rather than the one a
    /// capture's metadata carries, for sources the registry knows. The server
    /// viewer's `--registry-templates`. Takes effect at the next
    /// `init_templates` or `regenerate_combined`.
    pub fn set_prefer_registry_templates(&mut self, prefer: bool) {
        self.template_preference = preference(prefer);
    }

    /// Regenerate BOTH viewers' lazy `DashboardContext` using service
    /// extensions from BOTH attached captures and the explicitly named
    /// category template (when provided). When the experiment slot is
    /// empty, this is a no-op (the per-capture `init_templates` call
    /// already populated baseline's sections).
    ///
    /// Both slots get the same combined `DashboardContext`: compare-mode
    /// chart fetches query both slots for the active section route, so
    /// a category route like `/service/inference-library` must resolve
    /// in the experiment slot too — otherwise the experiment fetch
    /// 404s and the chart surfaces "Error: null".
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
        if self.experiment().is_none() || self.baseline.is_none() {
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
        // Notices are dropped here: each capture's `init_templates` already
        // returned them for the same metadata and registry.
        let preference = self.template_preference;
        let (baseline_exts, _) = self
            .baseline
            .as_ref()
            .map(|v| v.detect_and_validate_service_exts(&registry, preference))
            .unwrap_or_default();
        let (experiment_exts, _) = self
            .experiment()
            .map(|v| v.detect_and_validate_service_exts(&registry, preference))
            .unwrap_or_default();

        let mut service_exts: Vec<(String, dashboard::ServiceExtension)> = Vec::new();
        service_exts.extend(baseline_exts);
        service_exts.extend(experiment_exts);

        let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
            .iter()
            .map(|(name, ext)| (name.as_str(), ext))
            .collect();
        let service_names: std::collections::HashSet<&str> =
            service_exts.iter().map(|(name, _)| name.as_str()).collect();
        // Classify the baseline's sources (representative for compare mode) so
        // built-in gating matches the single-capture path. Immutable borrow —
        // released before the `as_mut()` assignments below.
        let sources = self
            .baseline
            .as_ref()
            .map(|b| classify_sources(b.reader.as_ref(), &b.file_metadata, &service_names))
            .unwrap_or_default();

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

        // Trimmed reports — if either slot carries the marker — collapse
        // both contexts to empty so the frontend lands on /report.
        let report_mode = self
            .baseline
            .as_ref()
            .map(|v| is_trimmed_report(&v.file_metadata))
            .unwrap_or(false)
            || self
                .experiment()
                .map(|v| is_trimmed_report(&v.file_metadata))
                .unwrap_or(false);
        // The built-in sections render from the baseline's template in both
        // slots, so the two share one navigation; its notice was returned by
        // the baseline's `init_templates`.
        let context = if report_mode {
            empty_dashboard_context()
        } else {
            let (template, _) = self
                .baseline
                .as_ref()
                .map(|b| dashboard_template(&b.file_metadata, preference))
                .expect("both captures are attached");
            let mut context = dashboard::dashboard::build_template_context(
                template,
                None,
                &service_refs,
                category,
                &sources,
            );
            if let Some(b) = self.baseline.as_ref() {
                let beside_notices = dashboard::dashboard::add_beside_sections(
                    &mut context,
                    b.reader.as_ref(),
                    &b.beside,
                    &registry,
                    preference,
                );
                self.notices.extend(beside_notices);
            }
            context
        };
        if let Some(baseline) = self.baseline.as_mut() {
            baseline.context = context.clone();
            baseline.cached_bodies.borrow_mut().clear();
        }
        if let Some(experiment) = self.experiment_mut() {
            experiment.context = context;
            experiment.cached_bodies.borrow_mut().clear();
        }
        Ok(())
    }

    pub fn get_sections(&self, capture: &str) -> Option<String> {
        self.slot(capture).map(|v| v.get_sections())
    }

    pub fn get_section(&self, capture: &str, section: &str) -> Option<String> {
        self.slot(capture).and_then(|v| v.get_section(section))
    }

    /// Produce the bytes that the server's `/api/v1/save_with_selection`
    /// would return: a `.dendro` report of the loaded recordings
    /// (`report_save::build_report`), converted in the browser from a `.rez`
    /// (tar or v3) or parquet source. A v3 `.rez` upload carries its own WAL
    /// table but not SQLite's `-wal` sidecar, so a copy taken from under a
    /// running recorder can hold fewer rows than `rezolus view` reads from
    /// the file. The JS caller
    /// wraps the bytes in a Blob and triggers a download — no HTTP needed.
    pub fn save_with_selection(&self, payload_json: &str) -> Result<Vec<u8>, JsValue> {
        use std::collections::BTreeSet;
        let payload: report_save::ReportPayload = serde_json::from_str(payload_json)
            .map_err(|e| JsValue::from_str(&format!("invalid selection payload: {e}")))?;
        let range = payload.time_range().map_err(|e| JsValue::from_str(&e))?;

        let baseline = self
            .baseline
            .as_ref()
            .ok_or_else(|| JsValue::from_str("no baseline capture attached"))?;

        let events_json = if payload.events.is_empty() {
            None
        } else {
            serde_json::to_string(&serde_json::json!({ "events": &payload.events })).ok()
        };

        // The kept columns across every attached capture — the baseline as
        // baseline, the rest as experiment — as the server unions them.
        let keep: Option<BTreeSet<String>> = payload.trim_columns.then(|| {
            let mut set: BTreeSet<String> = ::report_save::resolve_kept_columns(
                &payload,
                baseline.reader.as_ref(),
                ::report_save::Side::Baseline,
            )
            .into_iter()
            .collect();
            for (_, viewer) in &self.others {
                set.extend(::report_save::resolve_kept_columns(
                    &payload,
                    viewer.reader.as_ref(),
                    ::report_save::Side::Experiment,
                ));
            }
            set
        });

        let size = match &self.rez_source {
            Some(archive) => archive.len(),
            None => {
                baseline.source_bytes.len() + self.experiment().map_or(0, |e| e.source_bytes.len())
            }
        };
        if let Some(refusal) = report_too_large(size) {
            return Err(JsValue::from_str(&refusal));
        }

        let source = match (&self.rez_source, self.experiment()) {
            (Some(archive), _) => ::report_save::ReportSource::Dendro(archive),
            (None, Some(experiment)) => ::report_save::ReportSource::Compare {
                baseline: &baseline.source_bytes,
                experiment: &experiment.source_bytes,
            },
            (None, None) => ::report_save::ReportSource::Dendro(&baseline.source_bytes),
        };
        ::report_save::build_report(
            source,
            baseline.anchor_labels.as_ref(),
            keep.as_ref(),
            range,
            payload_json,
            events_json.as_deref(),
        )
        .map_err(|e| JsValue::from_str(&e))
    }

    /// The download extension a Save-as-Report produces: `.dendro`, the one
    /// container 6.x writes. The JS adapter uses it to name the download.
    pub fn report_extension(&self) -> String {
        ".dendro".to_string()
    }

    /// The attached capture with wire id `capture`, or `None`.
    fn slot(&self, capture: &str) -> Option<&Viewer> {
        if capture == BASELINE_ID {
            return self.baseline.as_ref();
        }
        self.others
            .iter()
            .find(|(id, _)| id == capture)
            .map(|(_, v)| v)
    }

    /// Mutable access to an already-attached capture by id.
    fn slot_by_id_mut(&mut self, capture: &str) -> Option<&mut Viewer> {
        if capture == BASELINE_ID {
            return self.baseline.as_mut();
        }
        self.others
            .iter_mut()
            .find(|(id, _)| id == capture)
            .map(|(_, v)| v)
    }

    /// Attach (or replace, by id) a capture. `baseline` fills the anchor; any
    /// other id replaces that capture in place or appends it, so re-uploading
    /// one arm does not reorder the rest.
    fn set_capture(&mut self, capture: &str, viewer: Viewer) {
        if capture == BASELINE_ID {
            self.baseline = Some(viewer);
            return;
        }
        match self.others.iter_mut().find(|(id, _)| id == capture) {
            Some((_, existing)) => *existing = viewer,
            None => self.others.push((capture.to_string(), viewer)),
        }
    }

    /// The conventional experiment — the first non-anchor capture — for the
    /// two-armed A/B operations that stay pairwise.
    fn experiment(&self) -> Option<&Viewer> {
        self.others.first().map(|(_, v)| v)
    }

    fn experiment_mut(&mut self) -> Option<&mut Viewer> {
        self.others.first_mut().map(|(_, v)| v)
    }

    fn require_slot(&self, capture: &str) -> Result<&Viewer, JsValue> {
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
/// PromQL query against the data source.
fn validate_service_extensions_inline(
    data: &dyn MetricsSource,
    exts: &mut [(String, dashboard::ServiceExtension)],
) {
    use metriken_query::QueryResult;
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

/// Enrich a file-metadata JSON map with pre-computed multi-node info.
///
/// Parses `per_source_metadata` and adds `nodes`, `node_versions`, and
/// `service_instances` so the frontend doesn't have to duplicate this logic.
fn enrich_with_multi_node_info(map: &mut serde_json::Map<String, serde_json::Value>) {
    let psm = match map.get(KEY_PER_SOURCE_METADATA).and_then(|v| v.as_object()) {
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

    /// Build a multi-recording dendro archive and hand back its bytes, the
    /// way a browser receives one: a blob, with no file behind it.
    fn archive_bytes(recordings: &[(&str, &str)]) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.dendro");
        rez::fixtures::multi_recording(&path, recordings);
        std::fs::read(&path).unwrap()
    }

    /// The committed `.rez` archive `name` upgraded to a `.dendro`, as
    /// bytes: what a user opens after `recording upgrade`.
    fn committed_rez_bytes(name: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let rez = rez::fixtures::committed_rez(name, dir.path());
        let out = dir.path().join("upgraded.dendro");
        metriken_storage::to_dendro::convert_path(&rez, &out).unwrap();
        std::fs::read(&out).unwrap()
    }

    /// A legacy upload is refused with the command that converts it on the
    /// user's machine; anything else is named as not a recording. (The
    /// refusal itself is a `JsValue`, which only a wasm32 build can make.)
    #[test]
    fn a_legacy_upload_names_the_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(rez::fixtures::committed_rez("v3.rez", dir.path())).unwrap();
        let container = rez::container::Container::of_bytes(&bytes);
        let msg = not_dendro("my run.rez", container);
        assert!(
            msg.ends_with("rezolus recording upgrade 'my run.rez'"),
            "{msg}"
        );
        assert!(
            not_dendro("x.txt", rez::container::Container::Unknown).contains("not a recording")
        );
    }

    /// A one-recording `.dendro`, as bytes, whose source is `source` and whose
    /// source metadata also holds `extra`.
    fn dendro_bytes(source: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        let mut sources = rez::fixtures::multi_recording_sources(&[(source, "web-01")]);
        sources[0].metadata.extend(rez::fixtures::map(extra));
        rez::fixtures::write_dendro(&path, &sources, None, rez::fixtures::Finish::Finalized);
        std::fs::read(&path).unwrap()
    }

    fn titled_template(service: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "service_name": service,
            "kpis": [{"role": "throughput", "title": title, "query": "up", "type": "gauge"}],
        })
    }

    /// The registry the static site would pass: two services, `known` and
    /// `other`.
    fn site_templates() -> String {
        serde_json::json!([
            titled_template("known", "from the registry"),
            titled_template("other", "from the registry"),
        ])
        .to_string()
    }

    /// The (source, first KPI title) pairs a capture rendered.
    fn rendered(reg: &WasmCaptureRegistry) -> Vec<(String, String)> {
        reg.baseline
            .as_ref()
            .unwrap()
            .context
            .service_exts
            .iter()
            .map(|(s, e)| (s.clone(), e.kpis[0].title.clone()))
            .collect()
    }

    /// A dendro source carrying a template for a service the registry does
    /// not know renders that template's section: the parity gap with the
    /// server viewer, which already read the metadata.
    #[test]
    fn a_dendro_sources_own_template_renders_for_an_unknown_service() {
        let carried = titled_template("acme", "carried").to_string();
        let mut reg = WasmCaptureRegistry::new();
        reg.attach(
            "baseline",
            &dendro_bytes("acme", &[("service_queries", &carried)]),
            "rec.dendro",
        )
        .unwrap();
        reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [("acme".to_string(), "carried".to_string())]
        );
        let sections: Vec<serde_json::Value> =
            serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
        assert!(
            sections.iter().any(|s| s["route"] == "/service/acme"),
            "the carried template's section is listed: {sections:?}"
        );
    }

    /// For a service the registry knows, the archive's template wins, and
    /// preferring the registry renders the registry's instead.
    #[test]
    fn the_archives_template_wins_unless_the_registry_is_preferred() {
        let carried = titled_template("known", "carried").to_string();
        let bytes = dendro_bytes("known", &[("service_queries", &carried)]);
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &bytes, "rec.dendro").unwrap();
        reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [("known".to_string(), "carried".to_string())]
        );

        reg.set_prefer_registry_templates(true);
        reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [("known".to_string(), "from the registry".to_string())]
        );
    }

    /// A capture whose `source` is a JSON array (a combined parquet's shape,
    /// here in a dendro source's metadata) with a top-level template: the
    /// template renders under its `service_name`, the other sources keep the
    /// registry's, and no section is named after the array.
    #[test]
    fn a_top_level_template_on_a_combined_source_is_keyed_by_its_service_name() {
        let psm = serde_json::json!({
            "known": {"0": {"version": "1"}},
            "other": {"0": {"version": "1"}},
            "rezolus": {"0": {"version": "6"}},
        })
        .to_string();
        let carried = titled_template("known", "carried").to_string();
        let bytes = dendro_bytes(
            "combined",
            &[
                ("source", r#"["known","other","rezolus"]"#),
                ("per_source_metadata", &psm),
                ("service_queries", &carried),
            ],
        );
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &bytes, "rec.dendro").unwrap();
        reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [
                ("known".to_string(), "carried".to_string()),
                ("other".to_string(), "from the registry".to_string()),
            ]
        );
        let sections = reg.get_sections("baseline").unwrap();
        assert!(!sections.contains("/service/["), "{sections}");

        reg.set_prefer_registry_templates(true);
        reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [
                ("known".to_string(), "from the registry".to_string()),
                ("other".to_string(), "from the registry".to_string()),
            ]
        );
    }

    /// A template in the metadata that does not parse falls back to the
    /// registry's, with a notice saying so, rather than failing the load.
    #[test]
    fn a_malformed_carried_template_falls_back_to_the_registry_with_a_notice() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach(
            "baseline",
            &dendro_bytes("known", &[("service_queries", r#"{"service_name": 7}"#)]),
            "rec.dendro",
        )
        .unwrap();
        let notices = reg.init_templates("baseline", &site_templates()).unwrap();
        assert_eq!(
            rendered(&reg),
            [("known".to_string(), "from the registry".to_string())]
        );
        let notices: Vec<String> = serde_json::from_str(&notices).unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("does not parse"), "{notices:?}");
    }

    /// Selection and events embedded in a recording's metadata (here a
    /// `.rez` recording's manifest, upgraded) are
    /// surfaced by the WASM backend with no rez-specific code: `Viewer` reads
    /// the whole `reader.file_metadata()` map, so `selection()` and
    /// `file_metadata_json()` return them the moment the manifest carries the
    /// keys — the browser-side half of the parity with the server's
    /// `init_file_mode_rez`.
    #[test]
    fn a_rez_with_manifest_selection_and_events_surfaces_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = rez::fixtures::committed_rez("v3-multi.rez", dir.path());
        // Embed into the anchor recording's manifest (id order is stable).
        rez::fixtures::add_rez_metadata(
            &path,
            0,
            &[
                ("selection", r#"{"entries":[{"query":"cpu_usage"}]}"#),
                (
                    "events",
                    r#"{"events":[{"timestamp":1000000000,"description":"rollout"}]}"#,
                ),
            ],
        );
        let upgraded = dir.path().join("fleet.dendro");
        metriken_storage::to_dendro::convert_path(&path, &upgraded).unwrap();
        let bytes = std::fs::read(&upgraded).unwrap();

        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &bytes, "fleet.dendro").unwrap();

        assert!(
            reg.selection("baseline")
                .is_some_and(|s| s.contains("cpu_usage")),
            "baseline selection must surface from the manifest"
        );
        let fm = reg.file_metadata_json("baseline").unwrap();
        assert!(
            fm.contains("rollout"),
            "baseline events must ride file_metadata: {fm}"
        );
    }

    /// An archive of two recordings attached as a comparison fills both
    /// slots — the same mapping `rezolus view --baseline .. --experiment ..`
    /// makes for the same file. Loading one arm into one slot would show an
    /// A/B archive as a single capture with nothing on screen saying half of
    /// it is missing.
    #[test]
    fn a_two_recording_archive_fills_both_capture_slots() {
        let mut reg = WasmCaptureRegistry::new();
        reg.set_compare_requested(true);
        reg.attach(
            "baseline",
            &archive_bytes(&[("redis", "web-01"), ("valkey", "web-01")]),
            "fleet.dendro",
        )
        .unwrap();

        assert!(reg.has("baseline") && reg.has("experiment"));
        // Identity, not just "two captures loaded": each slot must be the
        // recording it claims. The arms' own metadata is written by a
        // different path than their labels, so it is an independent witness.
        assert!(
            reg.file_metadata_json("baseline")
                .unwrap()
                .contains("redis")
        );
        assert!(
            reg.file_metadata_json("experiment")
                .unwrap()
                .contains("valkey")
        );
        // And named by the label that tells them apart — the shared rule the
        // server viewer uses, so one archive reads the same in both.
        assert_eq!(
            reg.slot("baseline").unwrap().alias.as_deref(),
            Some("redis")
        );
        assert_eq!(
            reg.slot("experiment").unwrap().alias.as_deref(),
            Some("valkey")
        );
        assert_eq!(reg.notices(), "[]");
    }

    /// A two-recording `.rez` fills both slots too, each recording named by
    /// the label that tells them apart.
    #[test]
    fn a_two_recording_rez_fills_both_capture_slots() {
        let mut reg = WasmCaptureRegistry::new();
        reg.set_compare_requested(true);
        reg.attach(
            "baseline",
            &committed_rez_bytes("v3-multi.rez"),
            "fleet.rez",
        )
        .unwrap();
        assert!(reg.has("baseline") && reg.has("experiment"));
        assert_eq!(
            reg.slot("baseline").unwrap().alias.as_deref(),
            Some("web-01")
        );
        assert_eq!(
            reg.slot("experiment").unwrap().alias.as_deref(),
            Some("web-02")
        );
        assert_eq!(reg.notices(), "[]");
    }

    /// An archive of two hosts of one run, from either entry point, is one
    /// combined capture: the built-in sections once per host, the second
    /// host's routes keyed `@h2`, each section's queries restricted to its
    /// host. `set_compare_requested` attaches it as a comparison instead.
    #[test]
    fn a_one_run_archive_is_one_combined_capture() {
        use rez::fixtures::{Recording, one_run};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.dendro");
        let host = |labels, cpus| Recording {
            labels,
            metadata: &[("source", "rezolus")],
            cpus,
            busy_ns: 500_000_000,
            counter: None,
        };
        one_run(
            &path,
            &[
                host(&[("source", "rezolus"), ("host", "h1")], 1),
                host(&[("source", "rezolus"), ("host", "h2")], 2),
            ],
        );
        let bytes = std::fs::read(&path).unwrap();

        let single = Viewer::new(&bytes, "run.dendro").expect("one run opens as one capture");
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &bytes, "run.dendro").unwrap();
        assert!(!reg.has("experiment"));
        // The archive answers each recording's catalog questions.
        assert!(single.catalog.is_some());
        assert!(reg.baseline.as_ref().unwrap().catalog.is_some());
        for sections in [single.get_sections(), reg.get_sections("baseline").unwrap()] {
            let sections: Vec<serde_json::Value> = serde_json::from_str(&sections).unwrap();
            let routes: Vec<&str> = sections
                .iter()
                .map(|s| s["route"].as_str().unwrap())
                .collect();
            assert!(
                routes.contains(&"/cpu") && routes.contains(&"/cpu@h2"),
                "{routes:?}"
            );
            assert_eq!(routes.iter().filter(|r| **r == "/query").count(), 1);
        }
        let cpu: serde_json::Value =
            serde_json::from_str(&reg.get_section("baseline", "cpu@h2").unwrap()).unwrap();
        assert_eq!(cpu["metadata"]["scope"][0]["value"], "h2");
        assert!(
            cpu.to_string().contains(r#"cpu_usage{host=\"h2\"}"#),
            "{cpu}"
        );

        let mut compare = WasmCaptureRegistry::new();
        compare.set_compare_requested(true);
        compare.attach("baseline", &bytes, "run.dendro").unwrap();
        assert!(compare.has("experiment"));
    }

    /// The arms order a comparison's captures; arms that cannot fill two
    /// slots are refused with the recordings listed.
    #[test]
    fn a_comparison_attaches_in_arm_order() {
        let l = |arm: &str| -> std::collections::BTreeMap<String, String> {
            [("arm".to_string(), arm.to_string())].into()
        };
        assert_eq!(
            comparison_order(&[l("experiment"), l("baseline")], "f").unwrap(),
            (vec![1, 0], vec![], None)
        );
        let unarmed: std::collections::BTreeMap<String, String> =
            [("source".to_string(), "x".to_string())].into();
        let one = comparison_order(&[l("baseline"), unarmed], "f.rez").unwrap_err();
        assert!(one.contains("one recording carries"), "{one}");
        let err = comparison_order(&[l("baseline"), l("baseline")], "f.rez").unwrap_err();
        assert!(
            err.contains("f.rez") && err.contains("arm=baseline"),
            "{err}"
        );
    }

    /// Above two, a comparison takes the first two recordings into its
    /// slots and shows the rest beside it, not as further arms: the
    /// baseline's source answers a query naming one of them by its key.
    #[test]
    fn recordings_past_the_two_slots_are_beside_not_arms() {
        let mut reg = WasmCaptureRegistry::new();
        reg.set_compare_requested(true);
        reg.attach(
            "baseline",
            &archive_bytes(&[
                ("redis", "web-01"),
                ("valkey", "web-01"),
                ("envoy", "web-01"),
            ]),
            "fleet.dendro",
        )
        .unwrap();
        let captures: Vec<serde_json::Value> = serde_json::from_str(&reg.captures()).unwrap();
        let ids: Vec<&str> = captures.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["baseline", "experiment"]);
        assert_eq!(
            reg.notices(),
            r#"["beside the comparison, each in its own sections: web-01"]"#
        );
        assert!(
            reg.file_metadata_json("baseline")
                .unwrap()
                .contains("redis")
        );
        assert!(
            reg.file_metadata_json("experiment")
                .unwrap()
                .contains("valkey")
        );
        let beside = &reg.baseline.as_ref().unwrap().beside;
        assert_eq!(beside.len(), 1);
        let (start, end) = beside[0].data.time_range().unwrap();
        let q = format!("irate(cpu_cycles{{__capture__=\"{}\"}}[2s])", beside[0].key);
        let got = reg
            .query_range("baseline", &q, start, end, 1.0, "grid")
            .unwrap();
        assert!(got.contains("envoy"), "{got}");
    }

    /// An archive that was never finalized must SAY so.
    ///
    /// This is the one place the browser can hold less than `rezolus view`
    /// would show for the same recording: a `.rez` carries its own unsealed
    /// rows in a `wal` table inside the file, but SQLite also commits into a
    /// `-wal` sidecar, and a browser is handed one file. Rendering short
    /// without saying so would look exactly like a recording that stopped
    /// early.
    #[test]
    fn an_unfinalized_archive_says_it_is_unfinalized() {
        // `v3-live.rez`: a consistent single file taken while its writer
        // held it (`VACUUM INTO`, the way `hindsight` took one), the copy a
        // browser can actually be handed. Not finalized.
        let snapshot_bytes = committed_rez_bytes("v3-live.rez");

        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &snapshot_bytes, "live.rez").unwrap();
        assert!(reg.has("baseline"));

        let notices: Vec<String> = serde_json::from_str(&reg.notices()).unwrap();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("not cleanly finalized"),
            "{}",
            notices[0]
        );
        assert!(
            notices[0].contains("-wal"),
            "the notice must name what a single file cannot carry: {}",
            notices[0]
        );
    }

    /// And a finalized one says nothing — a notice on every ordinary recording
    /// would train the reader to ignore the one that matters.
    #[test]
    fn a_finalized_archive_raises_no_notice() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach(
            "baseline",
            &archive_bytes(&[("redis", "web-01")]),
            "one.dendro",
        )
        .unwrap();
        assert_eq!(reg.notices(), "[]");
    }

    /// The registry holds more than two captures, each reachable by its own
    /// id — the storage generalization behind N-way compare. Attaching
    /// parquet uploads under distinct ids keeps them distinct.
    #[test]
    fn the_registry_holds_more_than_two_captures() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &one_recording(), "base.dendro")
            .unwrap();
        reg.attach("redis", &one_recording(), "redis.dendro")
            .unwrap();
        reg.attach("valkey", &one_recording(), "valkey.dendro")
            .unwrap();

        for id in ["baseline", "redis", "valkey"] {
            assert!(reg.has(id), "{id} must be attached");
            assert!(reg.slot(id).is_some(), "{id} must resolve to a viewer");
        }
        // Each is its own capture, not one shadowing another.
        assert_eq!(
            reg.slot("redis").unwrap().reader.filename().as_deref(),
            Some("redis.dendro")
        );
        assert_eq!(
            reg.slot("valkey").unwrap().reader.filename().as_deref(),
            Some("valkey.dendro")
        );

        // The two-armed experiment helper points at the FIRST non-anchor
        // capture — the classic A/B second arm the gated operations use.
        assert_eq!(
            reg.experiment().unwrap().reader.filename().as_deref(),
            Some("redis.dendro"),
        );

        // Detaching one leaves the rest addressable and in order.
        reg.detach("redis");
        assert!(!reg.has("redis"));
        assert!(reg.has("valkey"));
        assert_eq!(
            reg.experiment().unwrap().reader.filename().as_deref(),
            Some("valkey.dendro"),
            "the next non-anchor capture becomes the experiment",
        );
    }

    /// Re-attaching a capture by id replaces it in place rather than
    /// appending a duplicate, so re-uploading one arm does not reorder the rest.
    #[test]
    fn re_attaching_a_capture_by_id_replaces_in_place() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &one_recording(), "base.dendro")
            .unwrap();
        reg.attach("redis", &one_recording(), "old.dendro").unwrap();
        reg.attach("valkey", &one_recording(), "valkey.dendro")
            .unwrap();
        reg.attach("redis", &one_recording(), "new.dendro").unwrap();

        assert_eq!(reg.others.len(), 2, "no duplicate redis");
        assert_eq!(
            reg.slot("redis").unwrap().reader.filename().as_deref(),
            Some("new.dendro")
        );
        // redis stays first (its original position), valkey second.
        assert_eq!(reg.others[0].0, "redis");
        assert_eq!(reg.others[1].0, "valkey");
    }

    /// A single-recording archive is one capture, and must not conjure an
    /// experiment slot out of nothing.
    #[test]
    fn a_single_recording_archive_is_one_capture() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("baseline", &committed_rez_bytes("v3.rez"), "one.rez")
            .unwrap();
        assert!(reg.has("baseline"));
        assert!(!reg.has("experiment"));
    }

    /// A one-recording archive lands in the slot the caller asked for, and
    /// nothing else moves: the experiment of a two-file comparison.
    #[test]
    fn a_one_recording_archive_attaches_to_the_slot_it_was_given() {
        let mut reg = WasmCaptureRegistry::new();
        reg.attach("experiment", &one_recording(), "capture.dendro")
            .unwrap();
        assert!(reg.has("experiment"));
        assert!(!reg.has("baseline"));
    }

    /// A capture opened from its own one-recording archive keeps the
    /// archive and can be saved alone; one recording of a combined archive
    /// is saved through that archive instead.
    #[test]
    fn a_capture_of_its_own_archive_can_be_saved() {
        let one = Viewer::new(&one_recording(), "one.dendro").unwrap();
        assert!(one.can_save());
        let combined = Viewer::new(
            &archive_bytes(&[("redis", "web-01"), ("valkey", "web-01")]),
            "fleet.dendro",
        )
        .unwrap();
        assert!(!combined.can_save());
    }

    /// A one-recording dendro archive's bytes.
    fn one_recording() -> Vec<u8> {
        archive_bytes(&[("redis", "web-01")])
    }

    #[test]
    fn viewer_get_sections_empty_when_no_context() {
        // Default context = no templates loaded = empty nav.
        let ctx: dashboard::dashboard::DashboardContext = Default::default();
        let json = serde_json::to_string(&ctx.sections).unwrap();
        assert_eq!(json, "[]");
    }

    /// Save as Report refuses a recording over 1 GiB, naming the server
    /// viewer and the tools that make it smaller, and takes one at the limit.
    #[test]
    fn a_report_of_a_recording_over_a_gib_is_refused() {
        assert!(report_too_large(MAX_REPORT_SOURCE_BYTES).is_none());
        let refusal = report_too_large(MAX_REPORT_SOURCE_BYTES + 1).unwrap();
        assert!(refusal.contains("rezolus view"), "{refusal}");
        assert!(refusal.contains("rezolus recording upgrade"), "{refusal}");
    }

    /// Every report is a `.dendro`. A dendro source, a tar or v3 `.rez` and
    /// a parquet save one in the browser (no filesystem), the selection
    /// embedded on the anchor.
    #[test]
    fn every_source_saves_a_dendro_report() {
        use metriken_query::MetricsSource;
        let body = r#"{"entries":[],"trim_columns":false}"#;
        let saved = |bytes: &[u8], name: &str| {
            let mut reg = WasmCaptureRegistry::new();
            reg.attach("baseline", bytes, name).unwrap();
            assert_eq!(reg.report_extension(), ".dendro");
            // trim_columns=false keeps every column, so the report is a full
            // copy with the selection embedded — no query engine needed.
            let out = reg.save_with_selection(body).unwrap();
            assert!(metriken_storage::DendroCatalog::is_archive_bytes(&out));
            rez::reader::RezReader::open_recordings_from_bytes(out, BufferPool::new(8 << 20))
                .unwrap()
        };
        let report = saved(
            &archive_bytes(&[("redis", "web-01"), ("valkey", "web-01")]),
            "fleet.dendro",
        );
        assert_eq!(report.len(), 2, "the report keeps both recordings");
        assert!(
            report[0].1.file_metadata().contains_key("selection"),
            "the selection is embedded on the anchor"
        );
        assert_eq!(saved(&committed_rez_bytes("v2.rez"), "old.rez").len(), 1);
        assert_eq!(
            saved(&committed_rez_bytes("v3-multi.rez"), "multi.rez").len(),
            2
        );
        assert_eq!(saved(&one_recording(), "capture.dendro").len(), 1);

        // A v3 report holds every row the `.rez` reads: segments and WAL
        // rows (`v3-live.rez`), and WAL rows alone (`v3-wal-only.rez`).
        // Counted as the points of a 1 s rate over the whole recording,
        // whose rows are a second apart.
        let rows = |readers: &rez::reader::LabeledRecordings| -> usize {
            readers
                .iter()
                .map(|(_, r)| {
                    let (start, end) = r.time_range().unwrap();
                    let result = r.query_range("irate(ops[5s])", start, end, 1.0).unwrap();
                    serde_json::to_value(result).unwrap()["result"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s["values"].as_array().unwrap().len())
                        .sum::<usize>()
                })
                .sum()
        };
        for name in ["v3-live.rez", "v3-wal-only.rez"] {
            let bytes = committed_rez_bytes(name);
            let source = rez::reader::RezReader::open_recordings_from_bytes(
                bytes.clone(),
                BufferPool::new(8 << 20),
            )
            .unwrap();
            let want = rows(&source);
            assert!(want > 0, "{name}: the fixture holds rows");
            assert_eq!(rows(&saved(&bytes, name)), want, "{name}");
        }
    }

    /// The Rezolus template with `edit` applied to its CPU section.
    fn edited_cpu(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut doc: serde_json::Value =
            serde_json::from_str(&dashboard::rezolus_template::json()).unwrap();
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

    /// A registry with `dendro` attached as the baseline and its templates
    /// initialised; returns the init's notices.
    fn loaded(dendro: &[u8], prefer_registry: bool) -> (WasmCaptureRegistry, Vec<String>) {
        let mut reg = WasmCaptureRegistry::new();
        reg.set_prefer_registry_templates(prefer_registry);
        reg.attach("baseline", dendro, "rec.dendro").unwrap();
        let notices = reg.init_templates("baseline", "[]").unwrap();
        (reg, serde_json::from_str(&notices).unwrap())
    }

    fn cpu_name(reg: &WasmCaptureRegistry) -> String {
        let sections: Vec<serde_json::Value> =
            serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
        sections.iter().find(|s| s["route"] == "/cpu").unwrap()["name"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// With no `dashboard` in the recording, the navigation and every
    /// built-in section are what the generators render for it.
    #[test]
    fn a_recording_without_a_dashboard_renders_what_the_generators_render() {
        let (reg, notices) = loaded(&dendro_bytes("rezolus", &[]), false);
        assert!(notices.is_empty(), "{notices:?}");
        let viewer = reg.slot("baseline").unwrap();
        assert!(viewer.context.template.is_some());
        let sources = classify_sources(
            viewer.reader.as_ref(),
            &viewer.file_metadata,
            &std::collections::HashSet::new(),
        );
        let want_ctx = dashboard::dashboard::build_dashboard_context(None, &[], None, &sources);
        assert_eq!(
            reg.get_sections("baseline").unwrap(),
            serde_json::to_string(&want_ctx.sections).unwrap()
        );
        assert!(want_ctx.sections.iter().any(|s| s.route == "/cpu"));
        for section in &want_ctx.sections {
            let Some(mut want) = dashboard::dashboard::generate_section(
                viewer.reader.as_ref(),
                &section.route,
                &want_ctx,
            ) else {
                assert!(reg.get_section("baseline", &section.route[1..]).is_none());
                continue;
            };
            want.set_filename(viewer.reader.filename_or_default());
            let mut want = serde_json::to_value(&want).unwrap();
            strip_sections_from_section_body(&mut want);
            let got: serde_json::Value =
                serde_json::from_str(&reg.get_section("baseline", &section.route[1..]).unwrap())
                    .unwrap();
            assert_eq!(got, want, "{}", section.route);
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
        let dendro = dendro_bytes("rezolus", &[("dashboard", &doc)]);
        let first_group = |reg: &WasmCaptureRegistry| {
            let cpu: serde_json::Value =
                serde_json::from_str(&reg.get_section("baseline", "cpu").unwrap()).unwrap();
            cpu["groups"][0]["name"].as_str().unwrap().to_string()
        };
        let (reg, notices) = loaded(&dendro, false);
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(cpu_name(&reg), "Processors");
        assert_eq!(first_group(&reg), "Busy");
        let (reg, _) = loaded(&dendro, true);
        assert_eq!(cpu_name(&reg), "CPU");
        assert_eq!(first_group(&reg), "Utilization");
    }

    /// The page's copy of the metadata has no `dashboard`; the rest is kept.
    #[test]
    fn the_page_is_not_sent_the_section_template() {
        let (reg, _) = loaded(
            &dendro_bytes("rezolus", &[("dashboard", &renamed_cpu("Processors"))]),
            false,
        );
        let meta: serde_json::Value =
            serde_json::from_str(&reg.file_metadata_json("baseline").unwrap()).unwrap();
        assert!(meta.get("dashboard").is_none());
        assert_eq!(meta["source"], "rezolus");
        // The template still renders.
        assert_eq!(cpu_name(&reg), "Processors");
    }

    /// In a comparison both captures render the baseline's template, so they
    /// share one navigation.
    #[test]
    fn a_comparison_renders_the_baselines_dashboard_in_both_slots() {
        let renamed = dendro_bytes("rezolus", &[("dashboard", &renamed_cpu("Processors"))]);
        let plain = dendro_bytes("rezolus", &[]);
        for (baseline, experiment, want) in
            [(&renamed, &plain, "Processors"), (&plain, &renamed, "CPU")]
        {
            let mut reg = WasmCaptureRegistry::new();
            reg.attach("baseline", baseline, "a.dendro").unwrap();
            reg.attach("experiment", experiment, "b.dendro").unwrap();
            reg.init_templates("baseline", "[]").unwrap();
            reg.init_templates("experiment", "[]").unwrap();
            reg.regenerate_combined("[]", None).unwrap();
            for slot in ["baseline", "experiment"] {
                let sections: Vec<serde_json::Value> =
                    serde_json::from_str(&reg.get_sections(slot).unwrap()).unwrap();
                let cpu = sections.iter().find(|s| s["route"] == "/cpu").unwrap();
                assert_eq!(cpu["name"], want, "{slot}");
            }
        }
    }

    /// A `dashboard` that does not parse, or is over the size limit, falls
    /// back to the embedded template with a notice from `init_templates`.
    #[test]
    fn a_refused_dashboard_falls_back_with_a_notice() {
        let mut oversized = renamed_cpu("Processors");
        oversized.push_str(
            &" ".repeat(dashboard::source_templates::MAX_DASHBOARD_BYTES + 1 - oversized.len()),
        );
        for (doc, says) in [
            (
                r#"{"schema": 1, "sections": 7}"#.to_string(),
                "does not parse",
            ),
            (oversized, "over the"),
        ] {
            let (reg, notices) = loaded(&dendro_bytes("rezolus", &[("dashboard", &doc)]), false);
            assert_eq!(cpu_name(&reg), "CPU");
            assert_eq!(notices.len(), 1, "{notices:?}");
            assert!(notices[0].contains(says), "{notices:?}");
        }
    }

    /// A section the template cannot expand is an empty section carrying the
    /// reason; the other sections render.
    #[test]
    fn a_section_that_cannot_expand_is_empty_and_the_others_render() {
        let doc = edited_cpu(|cpu| {
            cpu["groups"] = serde_json::json!([{"name": "g", "id": "g", "subgroups": [{"plots": [
                {"kind": "gauge", "title": "t", "id": "p", "unit": "count", "query": "${throughput}"}
            ]}]}]);
        });
        let (reg, _) = loaded(&dendro_bytes("rezolus", &[("dashboard", &doc)]), false);
        let cpu: serde_json::Value =
            serde_json::from_str(&reg.get_section("baseline", "cpu").unwrap()).unwrap();
        assert_eq!(cpu["groups"], serde_json::json!([]));
        assert!(
            cpu["metadata"]["template_error"]
                .as_str()
                .unwrap()
                .contains("throughput")
        );
        let overview: serde_json::Value =
            serde_json::from_str(&reg.get_section("baseline", "overview").unwrap()).unwrap();
        assert!(overview["metadata"].get("template_error").is_none());
        assert!(!overview["groups"].as_array().unwrap().is_empty());
    }
}
