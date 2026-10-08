//! Action handlers — endpoints that mutate `AppState` (uploads, attach
//! and detach, live agent connect and reset, save). Live-mode recording is
//! in `live.rs`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use http::{header, StatusCode};
use reqwest::{Client, Url};
use tracing::{error, info, warn};

use metriken_query::{MetricsSource, ParquetReader};

use super::capture_registry::CaptureId;
use super::metadata::{
    build_multinode_systeminfo, classify_sources, compute_file_checksum, extract_parquet_metadata,
    extract_service_extension_metadata, regenerate_dashboards, validate_service_extensions,
};
use super::report_save;
use super::routes::on_query_slot;
use super::state::{ApiResponse, AppState, LazySectionStore};
use ::dashboard;

// ── Agent info ────────────────────────────────────────────────────────

/// Fetch the agent banner (`source version`) and `/systeminfo`. Used by
/// CLI startup and the runtime `/api/v1/connect` handler.
pub async fn fetch_agent_info(client: &Client, url: &Url) -> Result<AgentInfo, String> {
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("failed to connect to agent at {url}: {e}"))?;
    let banner = resp.text().await.unwrap_or_default();
    let first_line = banner.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let (source, version) = match parts.as_slice() {
        [name, ver, ..] => (name.to_string(), ver.to_string()),
        _ => {
            warn!("unexpected agent banner: {first_line:?}");
            ("rezolus".to_string(), String::new())
        }
    };

    let mut info_url = url.clone();
    info_url.set_path("/systeminfo");
    let sysinfo = match client.get(info_url).send().await {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        _ => None,
    };

    Ok(AgentInfo {
        source,
        version,
        sysinfo,
    })
}

#[derive(Clone)]
pub struct AgentInfo {
    pub source: String,
    pub version: String,
    pub sysinfo: Option<String>,
}

// ── Upload / load_url ─────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct LoadUrlBody {
    url: String,
    #[serde(default)]
    filename: Option<String>,
}

/// Fetch a remote parquet on the browser's behalf and ingest it. Refuses
/// every request when `--proxy-allow` was not set or when the host
/// doesn't match any allowlist pattern.
pub async fn load_url(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoadUrlBody>,
) -> Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("load_url is only available in file mode", "bad_request");
    }
    let Some(client) = state.proxy.client.as_ref() else {
        return ApiResponse::err("url loading is disabled", "forbidden");
    };

    let target = match Url::parse(&body.url) {
        Ok(u) => u,
        Err(e) => return ApiResponse::err(format!("invalid url: {e}"), "bad_request"),
    };
    if !matches!(target.scheme(), "http" | "https") {
        return ApiResponse::err("url scheme must be http or https", "bad_request");
    }
    let Some(host) = target.host_str().map(str::to_string) else {
        return ApiResponse::err("url is missing a host", "bad_request");
    };
    if !state.proxy.allow.allows(&host) {
        return ApiResponse::err(
            format!("host {host} not in --proxy-allow list"),
            "forbidden",
        );
    }

    let upstream = match client.get(target.clone()).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("load_url fetch failed for {target}: {e}");
            return ApiResponse::err(format!("upstream fetch failed: {e}"), "upstream_error");
        }
    };
    if !upstream.status().is_success() {
        return ApiResponse::err(
            format!("upstream returned {}", upstream.status()),
            "upstream_error",
        );
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => return ApiResponse::err(format!("upstream read failed: {e}"), "upstream_error"),
    };

    let filename = body.filename.unwrap_or_else(|| {
        target
            .path_segments()
            .and_then(|mut s| s.rfind(|seg| !seg.is_empty()))
            .map(ToString::to_string)
            .unwrap_or_else(|| "remote.parquet".to_string())
    });
    // Staging, opening and checksumming the file and building the
    // dashboards read the whole file, so they run in a query slot.
    on_query_slot(&state, move |state| {
        // Checked again here: a live connect can finish during the wait.
        if state.live.load(Ordering::Relaxed) {
            return ApiResponse::err("load_url is only available in file mode", "bad_request");
        }
        let temp_path = baseline_temp_path();
        if let Err(e) = std::fs::write(&temp_path, &bytes) {
            return ApiResponse::err(format!("failed to stage upstream bytes: {e}"), "io_error");
        }
        ingest_baseline_from_path(state, temp_path, filename)
    })
    .await
}

/// Upload and load a parquet file into file-mode viewer state.
pub async fn upload_parquet(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("upload is only available in file mode", "bad_request");
    }
    if body.is_empty() {
        return ApiResponse::err("missing parquet bytes", "bad_request");
    }

    let filename = filename_header(&headers).unwrap_or_else(|| "upload.parquet".to_string());
    // Storing, opening and checksumming the upload and building the
    // dashboards read the whole file, so they run in a query slot.
    on_query_slot(&state, move |state| ingest_upload(state, &body, filename)).await
}

/// Store an upload and load it as the baseline, by its content: a `.rez` or
/// `.dendro` archive, a combined-A/B tarball, or a parquet.
fn ingest_upload(
    state: &AppState,
    body: &[u8],
    filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    // Checked again here: a live connect can finish during the slot wait.
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("upload is only available in file mode", "bad_request");
    }
    let temp_path = baseline_temp_path();
    if let Err(e) = std::fs::write(&temp_path, body) {
        return ApiResponse::err(format!("failed to store upload: {e}"), "io_error");
    }
    // `.rez` (v2 tar) is also a tar, so check it before the A/B-tarball
    // sniffer. Dispatch is by CONTENT of the staged upload, not extension or
    // filename, and covers both `.rez` containers (v2 tar and v3 SQLite) —
    // `RezReader` dispatches on the container internally.
    if crate::recorder::rez::detect_rez_format(&temp_path)
        .unwrap_or(crate::recorder::rez::RezFormat::NotRez)
        != crate::recorder::rez::RezFormat::NotRez
    {
        return ingest_rez_from_path(state, temp_path, filename);
    }
    if super::ab_extract::looks_like_ab_tarball(&temp_path) {
        return ingest_combined_ab_from_path(state, temp_path, filename);
    }
    ingest_baseline_from_path(state, temp_path, filename)
}

/// Runtime-upload version of `init_file_mode_rez` (mod.rs): load a `.rez` as one
/// `RezReader` per recording, wiring a 2-recording archive onto the
/// baseline/experiment slots (>2 shows the first two). Returns an HTTP envelope.
fn ingest_rez_from_path(
    state: &AppState,
    rez_path: PathBuf,
    display_filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    use metriken_query::MetricsSource;

    let readers =
        match crate::rez_reader::RezReader::open_recordings(&rez_path, Arc::clone(&state.pool)) {
            Ok(r) if !r.is_empty() => r,
            Ok(_) => {
                let _ = std::fs::remove_file(&rez_path);
                return ApiResponse::err("empty .rez archive (no recordings)", "invalid_parquet");
            }
            Err(e) => {
                let _ = std::fs::remove_file(&rez_path);
                return ApiResponse::err(
                    format!("failed to read .rez archive: {e}"),
                    "invalid_parquet",
                );
            }
        };

    fn describe(reader: &crate::rez_reader::RezReader) -> (Option<String>, Option<String>) {
        use metriken_query::MetricsSource;
        let systeminfo = reader.metadata_get("systeminfo");
        let file_meta = {
            let mut map = serde_json::Map::new();
            for (k, v) in reader.file_metadata() {
                let jv = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v.clone()));
                map.insert(k, jv);
            }
            serde_json::to_string(&serde_json::Value::Object(map)).ok()
        };
        (systeminfo, file_meta)
    }
    // Same rule as the file-mode path: the alias must come from a label that
    // actually differs across these recordings, or a same-host A/B names both
    // arms identically.
    let alias_key = crate::viewer::discriminating_alias_key(
        &readers.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
    );
    let alias_of =
        |labels: &std::collections::BTreeMap<String, String>, fallback: &str| -> String {
            alias_key
                .as_ref()
                .and_then(|k| labels.get(k))
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };

    let n = readers.len();
    let mut readers = readers.into_iter();

    // Baseline = recording 0.
    let (b_labels, b_reader) = readers.next().expect("non-empty checked above");
    let (b_systeminfo, b_file_meta) = describe(&b_reader);
    let b_alias = alias_of(&b_labels, "baseline");
    state.replace_baseline(Arc::new(b_reader) as Arc<dyn MetricsSource>);
    *state.parquet_path.write() = Some(rez_path.clone());
    *state.experiment_parquet_path.write() = None;
    *state.cli_experiment_path.write() = None;
    state.captures.set_baseline_systeminfo(b_systeminfo);
    state.captures.set_baseline_file_metadata(b_file_meta);
    *state.file_checksum.write() = compute_file_checksum(&rez_path);
    state.captures.set_baseline_alias(Some(b_alias));

    // Experiment = recording 1, if present.
    if n >= 2 {
        let (e_labels, e_reader) = readers.next().expect("n >= 2");
        let (e_systeminfo, e_file_meta) = describe(&e_reader);
        let e_alias = alias_of(&e_labels, "experiment");
        state.captures.attach_experiment(
            Arc::new(e_reader) as Arc<dyn MetricsSource>,
            e_systeminfo,
            e_file_meta,
            Some(e_alias),
        );
        if n > 2 {
            warn!(
                "{n}-recording .rez uploaded: showing recordings 0 and 1 as \
                 baseline/experiment; N-way faceting is not yet supported"
            );
        }
    } else {
        state.captures.detach_experiment();
    }

    regenerate_dashboards(state);
    // Keep the uploaded .rez on disk (parquet_path references it), mirroring the
    // parquet upload path.
    ApiResponse::ok(serde_json::json!({ "filename": display_filename }))
}

/// Runtime-upload version of `init_file_mode_combined_ab` (mod.rs).
/// Mirrors the CLI state-mutation but returns an HTTP envelope instead
/// of exiting on failure, and skips strict `--category` validation
/// (manifest's category, if any, is applied as-is).
fn ingest_combined_ab_from_path(
    state: &AppState,
    tar_path: PathBuf,
    display_filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    let extracted = match super::ab_extract::extract_ab_tarball(&tar_path) {
        Ok(e) => e,
        Err(e) => {
            let _ = std::fs::remove_file(&tar_path);
            return ApiResponse::err(
                format!("failed to extract combined-A/B tarball: {e}"),
                "invalid_parquet",
            );
        }
    };
    let manifest = extracted.manifest.clone();

    let open = |label: &str, path: &std::path::Path| -> Result<Arc<dyn MetricsSource>, String> {
        ParquetReader::open_with_pool(path, Arc::clone(&state.pool))
            .map(|r| Arc::new(r.with_filename(display_filename.clone())) as Arc<dyn MetricsSource>)
            .map_err(|e| format!("failed to load {label} parquet from tarball: {e}"))
    };
    let baseline_reader = match open("baseline", &extracted.baseline_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&tar_path);
            return ApiResponse::err(e, "invalid_parquet");
        }
    };
    let experiment_reader = match open("experiment", &extracted.experiment_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&tar_path);
            return ApiResponse::err(e, "invalid_parquet");
        }
    };

    let (baseline_systeminfo, baseline_selection, baseline_file_meta) =
        extract_parquet_metadata(&extracted.baseline_path);
    let baseline_multinode = build_multinode_systeminfo(&extracted.baseline_path);
    let (experiment_systeminfo, _experiment_selection, experiment_file_meta) =
        extract_parquet_metadata(&extracted.experiment_path);
    let experiment_multinode = build_multinode_systeminfo(&extracted.experiment_path);
    let file_checksum = compute_file_checksum(&tar_path);

    state.replace_baseline(baseline_reader);
    *state.parquet_path.write() = Some(extracted.baseline_path.clone());
    *state.cli_experiment_path.write() = Some(extracted.experiment_path.clone());
    *state.experiment_parquet_path.write() = None;
    state
        .captures
        .set_baseline_systeminfo(baseline_multinode.or(baseline_systeminfo));
    *state.selection.write() = baseline_selection;
    *state.file_checksum.write() = file_checksum;
    state
        .captures
        .set_baseline_file_metadata(baseline_file_meta);
    *state.trimmed_report_marker.write() = super::read_footer_kv(
        &extracted.baseline_path,
        crate::parquet_metadata::KEY_REPORT,
    );
    state
        .captures
        .set_baseline_alias(Some(manifest.baseline.alias.clone()));
    state.captures.attach_experiment(
        experiment_reader,
        experiment_multinode.or(experiment_systeminfo),
        experiment_file_meta,
        Some(manifest.experiment.alias.clone()),
    );
    *state.category_name.write() = manifest.category.clone();
    *state.combined_ab_marker.write() = Some(manifest);

    // Keep the extracted tempdir alive — both per-side parquet paths
    // reference it. CLI does the same in `init_file_mode_combined_ab`.
    std::mem::forget(extracted);

    regenerate_dashboards(state);

    let _ = std::fs::remove_file(&tar_path);
    ApiResponse::ok(serde_json::json!({ "filename": display_filename }))
}

/// Shared baseline-ingest path used by upload and load_url. Takes
/// ownership of `temp_path`; the file is deleted on parquet-load
/// failure and retained on success (AppState references it).
pub fn ingest_baseline_from_path(
    state: &AppState,
    temp_path: PathBuf,
    filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    let reader = match ParquetReader::open_with_pool(&temp_path, Arc::clone(&state.pool)) {
        Ok(r) => r.with_filename(filename),
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return ApiResponse::err(format!("failed to load parquet: {e}"), "invalid_parquet");
        }
    };
    let filesize = std::fs::metadata(&temp_path).map(|m| m.len()).ok();
    let reader_arc: Arc<dyn MetricsSource> = Arc::new(reader);

    // Mirror the regenerate_dashboards short-circuit: a trimmed report
    // gets an empty section list so /api/v1/sections is consistent with
    // CLI-mode loading of the same parquet.
    let report_marker = super::read_footer_kv(&temp_path, crate::parquet_metadata::KEY_REPORT);
    let context = if report_marker.is_some() {
        ::dashboard::dashboard::DashboardContext {
            filesize,
            ..Default::default()
        }
    } else {
        let mut service_exts = extract_service_extension_metadata(&temp_path, &state.templates);
        validate_service_extensions(reader_arc.as_ref(), &mut service_exts);
        let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
        // Classify sources so an uploaded simple-capture parquet gets its
        // source: section here too — matching CLI load (regenerate_dashboards)
        // and the WASM viewer, all of which share this classifier.
        let sources = classify_sources(Some(&temp_path), reader_arc.as_ref(), &service_exts);
        ::dashboard::dashboard::build_dashboard_context(filesize, &service_refs, None, &sources)
    };
    let (systeminfo, selection, file_meta) = extract_parquet_metadata(&temp_path);
    let file_checksum = compute_file_checksum(&temp_path);

    let display_filename = reader_arc.filename_or_default();
    state.replace_baseline(reader_arc);
    *state.sections.write() = LazySectionStore::new(context);
    let multinode_sysinfo = build_multinode_systeminfo(&temp_path);
    *state.parquet_path.write() = Some(temp_path);
    *state.trimmed_report_marker.write() = report_marker;
    state
        .captures
        .set_baseline_systeminfo(multinode_sysinfo.or(systeminfo));
    *state.selection.write() = selection;
    *state.file_checksum.write() = file_checksum;
    state.captures.set_baseline_file_metadata(file_meta);

    ApiResponse::ok(serde_json::json!({ "filename": display_filename }))
}

pub fn baseline_temp_path() -> PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("rezolus-viewer-{}-{}", std::process::id(), suffix))
}

fn filename_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-rezolus-filename")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
}

// ── Experiment attach / detach ────────────────────────────────────────

/// Attach an experiment parquet for A/B comparison. Body is raw parquet
/// bytes. Returns 409 if an experiment is already attached.
pub async fn attach_experiment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.captures.experiment_attached() {
        return (
            StatusCode::CONFLICT,
            "experiment already attached; DELETE first",
        )
            .into_response();
    }
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing parquet bytes").into_response();
    }

    let filename = filename_header(&headers).unwrap_or_else(|| "experiment.parquet".to_string());
    // Writing the upload, opening it and rebuilding the dashboards (which run
    // KPI validation queries) are blocking file reads, so they run in a query
    // slot.
    on_query_slot(&state, move |state| attach_upload(state, &body, filename)).await
}

/// Store an uploaded experiment parquet and attach it.
fn attach_upload(state: &AppState, body: &[u8], filename: String) -> Response {
    // Checked again here: another attach can finish during the slot wait, and
    // both would write the same temp file.
    if state.captures.experiment_attached() {
        return (
            StatusCode::CONFLICT,
            "experiment already attached; DELETE first",
        )
            .into_response();
    }
    let temp_path =
        std::env::temp_dir().join(format!("rezolus-experiment-{}.parquet", std::process::id()));
    if let Err(e) = std::fs::write(&temp_path, body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store upload: {e}"),
        )
            .into_response();
    }

    let exp_reader = match ParquetReader::open_with_pool(&temp_path, Arc::clone(&state.pool)) {
        Ok(r) => r.with_filename(filename),
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return (
                StatusCode::BAD_REQUEST,
                format!("failed to load parquet: {e}"),
            )
                .into_response();
        }
    };

    let (sysinfo, _selection, file_meta) = extract_parquet_metadata(&temp_path);
    // HTTP-attached experiments don't carry an alias today; the
    // parameter is here so a future `x-rezolus-alias` header can thread
    // one through without further signature changes.
    state.unfollow_capture(super::capture_registry::EXPERIMENT_ID);
    state.captures.attach_experiment(
        Arc::new(exp_reader) as Arc<dyn MetricsSource>,
        sysinfo.clone(),
        file_meta,
        None,
    );
    *state.experiment_parquet_path.write() = Some(temp_path);

    regenerate_dashboards(state);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        sysinfo.unwrap_or_else(|| "{}".into()),
    )
        .into_response()
}

/// Detach the currently attached experiment (if any) and clean up its temp file.
///
/// All of it runs in a query slot, since rebuilding the dashboards runs KPI
/// validation queries; a request dropped while waiting for the slot changes
/// nothing.
pub async fn detach_experiment(State(state): State<Arc<AppState>>) -> Response {
    on_query_slot(&state, |state| {
        state.unfollow_capture(super::capture_registry::EXPERIMENT_ID);
        state.captures.detach_experiment();
        if let Some(path) = state.experiment_parquet_path.write().take() {
            let _ = std::fs::remove_file(&path);
        }
        // Clear the CLI-supplied experiment path too so regen below doesn't
        // rebuild against a detached capture. Only the path reference is
        // dropped — the user's parquet on disk is left alone.
        state.cli_experiment_path.write().take();
        regenerate_dashboards(state);
    })
    .await;
    StatusCode::OK.into_response()
}

// ── Live agent connect / reset ────────────────────────────────────────

/// The `live` flag, claimed for one connect. Released when dropped unless
/// [`keep`](Self::keep) was called.
struct LiveClaim<'a>(&'a std::sync::atomic::AtomicBool, bool);

impl<'a> LiveClaim<'a> {
    /// Set the flag, or `None` if it was already set.
    fn take(flag: &'a std::sync::atomic::AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag, false))
    }

    /// Leave the flag set.
    fn keep(mut self) {
        self.1 = true;
    }
}

impl Drop for LiveClaim<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(false, Ordering::Release);
        }
    }
}

/// Connect to a live Rezolus agent at runtime.
pub async fn connect_agent(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Json<ApiResponse<serde_json::Value>> {
    // Claimed before the awaits, so two connects racing cannot both start a
    // session; released on every error return.
    let Some(claim) = LiveClaim::take(&state.live) else {
        return ApiResponse::err("already connected to a live agent", "bad_request");
    };

    let url_str = match std::str::from_utf8(&body) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return ApiResponse::err("invalid UTF-8 in URL", "bad_request"),
    };
    let url: Url = match url_str.parse() {
        Ok(u) => u,
        Err(e) => return ApiResponse::err(format!("invalid URL: {e}"), "bad_request"),
    };

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            return ApiResponse::err(
                format!("failed to create HTTP client: {e}"),
                "internal_error",
            );
        }
    };

    let info = match fetch_agent_info(&client, &url).await {
        Ok(i) => i,
        Err(e) => return ApiResponse::err(e, "connection_error"),
    };

    let (source, version, sysinfo) = (
        info.source.clone(),
        info.version.clone(),
        info.sysinfo.clone(),
    );
    let session =
        match super::live::LiveSession::start(&client, &url, info, Arc::clone(&state.pool)).await {
            Ok(session) => session,
            Err(e) => return ApiResponse::err(e, "connection_error"),
        };
    let context = dashboard::dashboard::build_dashboard_context(None, &[], None, &[]);

    state.install_live(session);
    *state.sections.write() = LazySectionStore::new(context);
    state.captures.set_baseline_systeminfo(sysinfo);
    claim.keep();
    let info = AgentInfo {
        source,
        version,
        sysinfo: None,
    };

    info!(
        "Connected to {source} {version} at {url}",
        source = info.source,
        version = info.version
    );

    ApiResponse::ok(serde_json::json!({
        "source": info.source,
        "version": info.version,
        "url": url.to_string(),
    }))
}

/// Start a fresh live session against the same agent and install it; the
/// old session is dropped.
pub async fn reset_tsdb(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    if !state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("reset is only available in live mode", "bad_request");
    }

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            return ApiResponse::err(
                format!("failed to create HTTP client: {e}"),
                "internal_error",
            );
        }
    };
    // The old session is dropped once the new one is in place. That stops its
    // recording; its archive is deleted once its recording thread and any
    // save in progress release it.
    let target = state.live_session.lock().as_ref().map(|s| s.target());
    let Some((url, info)) = target else {
        return ApiResponse::err("no live agent is being recorded", "bad_request");
    };
    let session =
        match super::live::LiveSession::start(&client, &url, info, Arc::clone(&state.pool)).await {
            Ok(session) => session,
            Err(e) => return ApiResponse::err(e, "connection_error"),
        };
    state.install_live(session);
    info!("TSDB reset by user");
    ApiResponse::ok(serde_json::json!({ "ok": true }))
}

// ── Save ──────────────────────────────────────────────────────────────

fn parquet_attachment(filename: &str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(body))
        .unwrap()
}

fn server_error(msg: impl Into<String>) -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(msg.into()))
        .unwrap()
}

/// Save the live recording: a sealed copy of its archive, as a `.dendro`.
pub async fn save_capture(State(state): State<Arc<AppState>>) -> Response {
    let archive = state.live_session.lock().as_ref().map(|s| s.archive());
    let Some((path, hold)) = archive else {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    };
    let result = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dest = dir.path().join("capture.dendro");
        crate::hindsight::buffer::dump(
            &path,
            &dest,
            &crate::hindsight::state::TimeRange::new(None, None),
        )?;
        std::fs::read(&dest).map_err(|e| e.to_string())
    })
    .await;
    finalize_attachment(result, "rezolus-capture.dendro", parquet_attachment)
}

/// File mode: column-trim the loaded parquet (or repack a combined-A/B
/// tarball with per-side trims) using the saved selection, embed the
/// selection JSON in the output footer, and stream it back. Live mode takes
/// the archive branch: `parquet_path` is the live archive, and the report is
/// a trimmed `.dendro`.
///
/// The report is built in a query slot: resolving the kept columns opens
/// the recording's tables, and building the report reads all of it.
pub async fn save_with_selection(State(state): State<Arc<AppState>>, body: String) -> Response {
    // In live mode `parquet_path` is the live archive: hold its directory
    // until the report is built, so a reset meanwhile does not delete it.
    // Both are read under the session lock, which `install_live` holds while
    // it changes them.
    let (parquet_path, live_hold) = {
        let session = state.live_session.lock();
        (
            state.parquet_path.read().clone(),
            session.as_ref().map(|s| s.archive().1),
        )
    };
    // Live mode sets `parquet_path` to its archive; with no file and no live
    // agent there is nothing to save.
    let Some(path) = parquet_path else {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    };
    let selection_json = body;
    let payload: report_save::ReportPayload = match serde_json::from_str(&selection_json) {
        Ok(p) => p,
        Err(e) => {
            return ApiResponse::<()>::err(format!("invalid selection payload: {e}"), "bad_data")
                .into_response();
        }
    };
    let range = match payload.time_range() {
        Ok(r) => r,
        Err(e) => return ApiResponse::<()>::err(e, "bad_data").into_response(),
    };
    on_query_slot(&state, move |state| {
        let _hold = live_hold;
        // A panic is reported as a failed save, as a failed build is.
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build_report(state, &path, &payload, &selection_json, range)
        }));
        let (result, filename) = match built {
            Ok((result, filename)) => (Ok(result), filename),
            Err(_) => (Err("the report build panicked"), "rezolus-report"),
        };
        finalize_attachment(result, filename, parquet_attachment)
    })
    .await
}

/// Build the report for the recording at `path` and name its download: a
/// `.rez` or `.dendro` for an archive source or a parquet compare, a
/// `.parquet` for a single parquet.
fn build_report(
    state: &AppState,
    path: &std::path::Path,
    payload: &report_save::ReportPayload,
    selection_json: &str,
    range: Option<::report_save::TimeRange>,
) -> (Result<Vec<u8>, String>, &'static str) {
    let trim_columns = payload.trim_columns;
    let events_json = if payload.events.is_empty() {
        None
    } else {
        serde_json::to_string(&serde_json::json!({ "events": &payload.events })).ok()
    };

    // `.rez` source: save a trimmed `.rez` report (single recording or a
    // 2-recording A/B stay one archive), NOT a parquet or a
    // `.parquet.ab.tar`. This must come before the compare/single branches
    // below, which assume a parquet source and would fail trying to reparse
    // the SQLite container as parquet.
    if crate::recorder::rez::detect_rez_format(path)
        .map(|f| f != crate::recorder::rez::RezFormat::NotRez)
        .unwrap_or(false)
    {
        // Union the kept columns across every attached capture — the anchor
        // as baseline, the rest as experiment — so a 2-recording A/B keeps
        // both sides' queried metrics. A slightly looser set than a
        // per-recording trim, but never lossy.
        let keep: Option<std::collections::BTreeSet<String>> = trim_columns.then(|| {
            let mut set = std::collections::BTreeSet::new();
            for (i, id) in state.captures.capture_ids().into_iter().enumerate() {
                if let Some(source) = state.captures.get_by_id(&id) {
                    let side = if i == 0 {
                        ::report_save::Side::Baseline
                    } else {
                        ::report_save::Side::Experiment
                    };
                    set.extend(::report_save::resolve_kept_columns(
                        payload,
                        source.as_ref(),
                        side,
                    ));
                }
            }
            set
        });
        let result = super::report_save_rez::build_rez_report(
            path,
            keep.as_ref(),
            range,
            selection_json,
            events_json.as_deref(),
        );
        let dendro = metriken_archive::DendroCatalog::is_archive(path).unwrap_or(false);
        let name = if dendro {
            "rezolus-report.dendro"
        } else {
            "rezolus-report.rez"
        };
        return (result, name);
    }

    // Compare mode (two parquet sources): assemble a 2-recording `.rez`
    // report, one recording per side, instead of a `.parquet.ab.tar`. A
    // `.rez` source never reaches here — it returned above. Labels come from
    // each parquet's own source metadata, so there is no manifest to
    // synthesize.
    let baseline_data = state.baseline_data();
    let experiment_path = state.resolve_experiment_parquet_path();
    let experiment_data = state.captures.get(CaptureId::Experiment);
    if let (Some(experiment_path), Some(experiment_data)) = (experiment_path, experiment_data) {
        let keep = |data: &dyn MetricsSource, side| -> Option<std::collections::BTreeSet<String>> {
            trim_columns.then(|| {
                ::report_save::resolve_kept_columns(payload, data, side)
                    .into_iter()
                    .collect()
            })
        };
        let baseline_keep = keep(baseline_data.as_ref(), ::report_save::Side::Baseline);
        let experiment_keep = keep(experiment_data.as_ref(), ::report_save::Side::Experiment);
        let result = (|| -> Result<Vec<u8>, String> {
            let baseline_bytes =
                std::fs::read(path).map_err(|e| format!("failed to read baseline: {e}"))?;
            let experiment_bytes = std::fs::read(&experiment_path)
                .map_err(|e| format!("failed to read experiment: {e}"))?;
            let sides = [
                ::report_save::ParquetReportSide {
                    bytes: &baseline_bytes,
                    keep_metrics: baseline_keep.as_ref(),
                },
                ::report_save::ParquetReportSide {
                    bytes: &experiment_bytes,
                    keep_metrics: experiment_keep.as_ref(),
                },
            ];
            ::report_save::build_rez_report_from_parquets(
                &sides,
                trim_columns,
                range,
                selection_json,
                events_json.as_deref(),
            )
        })();
        return (result, "rezolus-report.rez");
    }

    // Single-capture save.
    let result = report_save::save_single_parquet(
        path,
        payload,
        selection_json,
        baseline_data.as_ref(),
        trim_columns,
    )
    .map_err(|e| e.to_string());
    (result, "rezolus-report.parquet")
}

/// Convert a blocking build's outcome into a download Response, logging
/// success and the two failure modes (build error vs. panic).
fn finalize_attachment<E: std::fmt::Display>(
    result: Result<Result<Vec<u8>, String>, E>,
    filename: &'static str,
    attach: fn(&str, Vec<u8>) -> Response,
) -> Response {
    match result {
        Ok(Ok(output)) => {
            info!("saved report {filename} ({} bytes)", output.len());
            attach(filename, output)
        }
        Ok(Err(e)) => {
            error!("report build failed: {e}");
            server_error(format!("report build failed: {e}"))
        }
        Err(e) => {
            error!("report task panicked: {e}");
            server_error("internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::dashboard::TemplateRegistry;

    fn upload_only_state() -> Arc<AppState> {
        let store: Arc<dyn MetricsSource> =
            Arc::new(metriken_query::MemoryStore::builder().build());
        Arc::new(AppState::new(store, TemplateRegistry::empty()))
    }

    /// Save as Report of a `.rez` source returns a `.rez` report carrying the
    /// selection, through the handler.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn save_with_selection_returns_a_rez_report_for_a_rez_source() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.rez");
        populated_v3_rez(&src, "baseline", &["cpu_usage"], 4);
        let state = upload_only_state();
        *state.parquet_path.write() = Some(src);

        let body = r#"{"entries":[],"trim_columns":false}"#;
        let response = save_with_selection(State(Arc::clone(&state)), body.to_string()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let disposition = response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(disposition.contains("rezolus-report.rez"), "{disposition}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let db = crate::recorder::rez_sqlite::RezDb::open_bytes(bytes.to_vec()).unwrap();
        let md = &db.read_recordings().unwrap()[0].meta.metadata;
        assert_eq!(
            md.get(crate::parquet_metadata::KEY_SELECTION)
                .map(String::as_str),
            Some(body)
        );
    }

    /// `upload_parquet` must dispatch a v3 (SQLite) `.rez` upload to
    /// `ingest_rez_from_path`, not the plain-parquet ingest path. Mutation
    /// check: reverting the `detect_rez_format` check on the staged upload to
    /// `is_rez_path` makes this fail — the sniff runs on CONTENT, not
    /// filename, so a v3 upload then falls through to
    /// `ingest_baseline_from_path`, which tries to open the SQLite bytes as a
    /// bare parquet and returns an `invalid_parquet` error instead of
    /// `success`.
    #[tokio::test]
    async fn upload_parquet_accepts_v3_sqlite_rez() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("rec.rez");
        crate::recorder::rez::recorder_tests_support::empty_v3_rez(&rez_path);
        assert_eq!(
            crate::recorder::rez::detect_rez_format(&rez_path).unwrap(),
            crate::recorder::rez::RezFormat::V3Sqlite,
            "fixture sanity: must actually be a v3 SQLite archive"
        );
        let bytes = std::fs::read(&rez_path).unwrap();

        let state = upload_only_state();
        let response = upload_parquet(State(state), HeaderMap::new(), Bytes::from(bytes)).await;
        let value = serde_json::to_value(&response.0).unwrap();
        assert_eq!(
            value["status"], "success",
            "upload_parquet must accept a v3 .rez upload: {value}"
        );
    }
}
