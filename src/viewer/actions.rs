//! Action handlers — endpoints that mutate `AppState` (uploads, attach
//! and detach, live agent connect, parquet save) plus the live-mode
//! ingest loop.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use http::{header, StatusCode};
use parking_lot::{Mutex, RwLock};
use reqwest::{Client, Url};
use tracing::{debug, error, info, warn};

use metriken_query::{MemoryStore, MetricsSource, ParquetReader};

use super::capture_registry::CaptureId;
use super::metadata::{
    build_multinode_systeminfo, compute_file_checksum, extract_parquet_metadata,
    extract_service_extension_metadata, regenerate_dashboards, validate_service_extensions,
};
use super::report_save;
use super::state::{ApiResponse, AppState, LazySectionStore};
use ::dashboard;

// ── Snapshot ingest (live mode) ───────────────────────────────────────

/// Background task that polls a live agent and ingests snapshots.
pub async fn ingest_loop(
    url: Url,
    store: MemoryStore,
    snapshots: Arc<Mutex<VecDeque<Vec<u8>>>>,
    source: String,
    version: String,
) {
    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("failed to create http client: {e}");
            return;
        }
    };

    // source/version/interval already set during construction; update in case
    // they changed (connect_agent path).
    store.set_source(&source);
    store.set_version(&version);
    store.set_sampling_interval_ms(1000);

    let interval_duration = Duration::from_secs(1);
    let mut interval = crate::common::aligned_interval(interval_duration);
    let mut sample_count: u64 = 0;

    loop {
        interval.tick().await;

        let start = Instant::now();
        let response = match client.get(url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("failed to fetch metrics: {e}");
                continue;
            }
        };
        let body = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                warn!("failed to read response body: {e}");
                continue;
            }
        };

        debug!("sampling latency: {} us", start.elapsed().as_micros());

        let snapshot: metriken_exposition::Snapshot = match rmp_serde::from_slice(&body) {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to deserialize snapshot: {e}");
                continue;
            }
        };

        store.ingest_snapshot(snapshot);
        sample_count += 1;

        snapshots.lock().push_back(body.to_vec());

        if sample_count <= 5 || sample_count.is_multiple_of(60) {
            debug!(
                "ingested {} samples, counters: {}, gauges: {}, histograms: {}",
                sample_count,
                store.counter_names().len(),
                store.gauge_names().len(),
                store.histogram_names().len(),
            );
        }
    }
}

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

    let temp_path = baseline_temp_path();
    if let Err(e) = std::fs::write(&temp_path, &bytes) {
        return ApiResponse::err(format!("failed to stage upstream bytes: {e}"), "io_error");
    }

    let filename = body.filename.unwrap_or_else(|| {
        target
            .path_segments()
            .and_then(|mut s| s.rfind(|seg| !seg.is_empty()))
            .map(ToString::to_string)
            .unwrap_or_else(|| "remote.parquet".to_string())
    });
    ingest_baseline_from_path(&state, temp_path, filename)
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
    let temp_path = baseline_temp_path();
    if let Err(e) = std::fs::write(&temp_path, &body) {
        return ApiResponse::err(format!("failed to store upload: {e}"), "io_error");
    }
    ingest_baseline_from_path(&state, temp_path, filename)
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
        ::dashboard::dashboard::build_dashboard_context(filesize, &service_refs, None)
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
    let temp_path =
        std::env::temp_dir().join(format!("rezolus-experiment-{}.parquet", std::process::id()));
    if let Err(e) = std::fs::write(&temp_path, &body) {
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
    state.captures.attach_experiment(
        Arc::new(exp_reader) as Arc<dyn MetricsSource>,
        sysinfo.clone(),
        file_meta,
        None,
    );
    *state.experiment_parquet_path.write() = Some(temp_path);

    regenerate_dashboards(&state);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        sysinfo.unwrap_or_else(|| "{}".into()),
    )
        .into_response()
}

/// Detach the currently attached experiment (if any) and clean up its temp file.
pub async fn detach_experiment(State(state): State<Arc<AppState>>) -> Response {
    state.captures.detach_experiment();
    if let Some(path) = state.experiment_parquet_path.write().take() {
        let _ = std::fs::remove_file(&path);
    }
    // Clear the CLI-supplied experiment path too so regen below doesn't
    // rebuild against a detached capture. Only the path reference is
    // dropped — the user's parquet on disk is left alone.
    state.cli_experiment_path.write().take();
    regenerate_dashboards(&state);
    StatusCode::OK.into_response()
}

// ── Live agent connect / reset ────────────────────────────────────────

/// Connect to a live Rezolus agent at runtime.
pub async fn connect_agent(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("already connected to a live agent", "bad_request");
    }

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

    let new_store = MemoryStore::builder()
        .source(info.source.clone())
        .version(info.version.clone())
        .sampling_interval_ms(1000)
        .filename(url.to_string())
        .build();
    let new_store_arc: Arc<dyn MetricsSource> = Arc::new(new_store.clone());
    let context = dashboard::dashboard::build_dashboard_context(None, &[], None);

    state.replace_baseline(new_store_arc);
    *state.sections.write() = LazySectionStore::new(context);
    state.captures.set_baseline_systeminfo(info.sysinfo);
    state.live.store(true, Ordering::Relaxed);

    let ingest_snapshots = state.snapshots.clone();
    let mut ingest_url = url.clone();
    ingest_url.set_path("/metrics/binary");

    tokio::spawn(ingest_loop(
        ingest_url,
        new_store,
        ingest_snapshots,
        info.source.clone(),
        info.version.clone(),
    ));

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

/// Reset the TSDB — clears all data and buffered snapshots.
pub async fn reset_tsdb(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    if !state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("reset is only available in live mode", "bad_request");
    }

    let data = state.baseline_data();
    let source = data.source();
    let version = data.version();
    let filename = data.filename_or_default();

    let new_store = MemoryStore::builder()
        .source(source)
        .version(version)
        .sampling_interval_ms(1000)
        .filename(filename)
        .build();
    let new_store_arc: Arc<dyn MetricsSource> = Arc::new(new_store);
    state.replace_baseline(new_store_arc);

    state.snapshots.lock().clear();
    info!("TSDB reset by user");
    ApiResponse::ok(serde_json::json!({ "ok": true }))
}

// ── Save parquet ──────────────────────────────────────────────────────

/// Convert buffered live-mode snapshots to a parquet byte vec, stamped
/// with `sampling_interval_ms`, optional `systeminfo`, and an optional
/// `selection` JSON.
fn snapshots_to_parquet(
    snapshot_data: Vec<Vec<u8>>,
    sysinfo_json: Option<String>,
    selection_json: Option<String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use std::io::Cursor;

    let total_size: usize = snapshot_data.iter().map(|s| s.len()).sum();
    let mut raw = Vec::with_capacity(total_size);
    for snapshot_bytes in &snapshot_data {
        raw.extend_from_slice(snapshot_bytes);
    }

    let reader = Cursor::new(raw);
    let mut output = Vec::new();
    let mut converter = metriken_exposition::MsgpackToParquet::with_options(
        metriken_exposition::ParquetOptions::new(),
    )
    .metadata("sampling_interval_ms".to_string(), "1000".to_string());

    if let Some(json) = sysinfo_json {
        converter = converter.metadata("systeminfo".to_string(), json);
    }
    if let Some(selection) = selection_json {
        converter = converter.metadata("selection".to_string(), selection);
    }

    converter
        .convert_file_handle(reader, Cursor::new(&mut output))
        .map(|rows| {
            info!("saved parquet with {rows} rows");
            output
        })
        .map_err(Into::into)
}

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

/// Save buffered live-mode snapshots as a parquet file download.
pub async fn save_parquet(State(state): State<Arc<AppState>>) -> Response {
    let snapshot_data: Vec<Vec<u8>> = state.snapshots.lock().iter().cloned().collect();
    if snapshot_data.is_empty() {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    }

    let sysinfo_json = state.captures.systeminfo(CaptureId::Baseline);
    let result = tokio::task::spawn_blocking(move || {
        snapshots_to_parquet(snapshot_data, sysinfo_json, None)
    })
    .await;

    match result {
        Ok(Ok(output)) => parquet_attachment("rezolus-capture.parquet", output),
        Ok(Err(e)) => {
            error!("failed to convert to parquet: {e}");
            server_error(format!("parquet conversion failed: {e}"))
        }
        Err(e) => {
            error!("parquet conversion task panicked: {e}");
            server_error("internal error")
        }
    }
}

/// File mode: column-trim the loaded parquet (or repack a combined-A/B
/// tarball with per-side trims) using the saved selection, embed the
/// selection JSON in the output footer, and stream it back. Live mode:
/// convert buffered snapshots into a parquet stamped with the selection
/// (no trim — there's no source parquet to project from).
pub async fn save_with_selection(State(state): State<Arc<AppState>>, body: String) -> Response {
    let parquet_path = state.parquet_path.read().clone();
    let selection_json = body;

    if let Some(path) = parquet_path {
        let payload: report_save::ReportPayload = match serde_json::from_str(&selection_json) {
            Ok(p) => p,
            Err(e) => {
                return ApiResponse::<()>::err(
                    format!("invalid selection payload: {e}"),
                    "bad_data",
                )
                .into_response();
            }
        };
        let baseline_data = state.baseline_data();
        let trim_columns = payload.trim_columns;
        // Bind to a local so the temporary read guard from .read() doesn't
        // extend through the `if let` body and trip Send across the await.
        let ab_manifest = state.combined_ab_marker.read().clone();

        if let Some(manifest) = ab_manifest {
            // parquet_path here is the EXTRACTED baseline parquet (set by
            // init_file_mode_combined_ab); the experiment side lives at
            // cli_experiment_path. Both paths outlive the process via the
            // mem::forget'd extractor handle.
            let Some(experiment_path) = state.cli_experiment_path.read().clone() else {
                return ApiResponse::<()>::err(
                    "combined-A/B state missing experiment_path",
                    "internal_error",
                )
                .into_response();
            };
            let experiment_data = state
                .captures
                .get(CaptureId::Experiment)
                .expect("experiment slot must be attached in combined-A/B mode");
            let result = tokio::task::spawn_blocking({
                let baseline_path = path.clone();
                let body = selection_json.clone();
                move || {
                    report_save::save_combined_ab_tarball(
                        &baseline_path,
                        &experiment_path,
                        &payload,
                        &body,
                        &manifest,
                        trim_columns,
                        baseline_data.as_ref(),
                        experiment_data.as_ref(),
                    )
                    .map_err(|e| e.to_string())
                }
            })
            .await;
            return finalize_report_attachment_tarball(result);
        }

        let result = tokio::task::spawn_blocking({
            let body = selection_json.clone();
            move || {
                report_save::save_single_parquet(
                    &path,
                    &payload,
                    &body,
                    baseline_data.as_ref(),
                    trim_columns,
                )
                .map_err(|e| e.to_string())
            }
        })
        .await;
        return finalize_report_attachment(result);
    }

    // Live mode: convert snapshots with the selection metadata.
    let snapshot_data: Vec<Vec<u8>> = state.snapshots.lock().iter().cloned().collect();
    if snapshot_data.is_empty() {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    }

    let sysinfo_json = state.captures.systeminfo(CaptureId::Baseline);
    let result = tokio::task::spawn_blocking(move || {
        snapshots_to_parquet(snapshot_data, sysinfo_json, Some(selection_json))
    })
    .await;

    match result {
        Ok(Ok(output)) => parquet_attachment("rezolus-capture-annotated.parquet", output),
        Ok(Err(e)) => {
            error!("failed to convert to parquet: {e}");
            server_error(format!("parquet conversion failed: {e}"))
        }
        Err(e) => {
            error!("parquet conversion task panicked: {e}");
            server_error("internal error")
        }
    }
}

fn finalize_report_attachment(
    result: Result<Result<Vec<u8>, String>, tokio::task::JoinError>,
) -> Response {
    finalize_attachment(result, "rezolus-report.parquet", parquet_attachment)
}

fn finalize_report_attachment_tarball(
    result: Result<Result<Vec<u8>, String>, tokio::task::JoinError>,
) -> Response {
    finalize_attachment(result, "rezolus-report.parquet.ab.tar", tar_attachment)
}

/// Convert a `spawn_blocking` outcome into a download Response, logging
/// success and the two failure modes (build error vs. task panic).
fn finalize_attachment(
    result: Result<Result<Vec<u8>, String>, tokio::task::JoinError>,
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

fn tar_attachment(filename: &str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-tar")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(body))
        .unwrap()
}
