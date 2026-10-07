//! HTTP routing and read-side handlers.
//!
//! Action handlers (upload, attach, save, connect, ingest, …) live in
//! `super::actions` and are wired in here.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use http::{header, StatusCode};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;
use tower_livereload::LiveReloadLayer;
use tracing::warn;

#[cfg(not(feature = "developer-mode"))]
use http::{HeaderMap, Uri};
#[cfg(not(feature = "developer-mode"))]
use include_dir::{include_dir, Dir};

#[cfg(feature = "developer-mode")]
use std::path::Path;
#[cfg(feature = "developer-mode")]
use tower_http::services::{ServeDir, ServeFile};

use std::sync::atomic::Ordering;

use dashboard::display_wire;
use metriken_query::{QueryError, QueryResult};

use super::actions;
use super::capture_registry::{self};
use super::state::{self, ApiResponse, AppState, CaptureParam};

#[cfg(not(feature = "developer-mode"))]
static ASSETS: Dir<'_> = include_dir!("src/viewer/assets");

pub fn app(livereload: LiveReloadLayer, app_state: AppState) -> Router {
    let app_state = Arc::new(app_state);

    // API routes get Cache-Control: no-store to prevent browsers from
    // returning stale data during live mode polling.
    let api_routes = Router::new()
        .route("/query", get(instant_query))
        .route("/query_range", get(range_query))
        .route("/labels", get(label_names))
        .route("/label/{name}/values", get(label_values))
        .route("/metadata", get(metadata))
        .route("/mode", get(mode))
        .route("/reset", axum::routing::post(actions::reset_tsdb))
        .route("/save", get(actions::save_capture))
        .route("/systeminfo", get(systeminfo_handler))
        .route("/selection", get(selection_handler))
        .route("/sections", get(sections_handler))
        .route("/file_metadata", get(file_metadata_handler))
        .route("/captures", get(captures_handler))
        .route("/metrics", get(metrics_handler))
        .route("/timestamps", get(timestamps_handler))
        .route(
            "/upload",
            axum::routing::post(actions::upload_parquet)
                .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route(
            "/captures/experiment",
            axum::routing::post(actions::attach_experiment)
                .delete(actions::detach_experiment)
                .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route("/connect", axum::routing::post(actions::connect_agent))
        .route(
            "/save_with_selection",
            axum::routing::post(actions::save_with_selection),
        )
        .route("/load_url", axum::routing::post(actions::load_url))
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                response
            },
        ));

    let router = Router::new()
        .route("/about", get(about))
        .route("/data/{*path}", get(data))
        .nest("/api/v1", api_routes)
        .with_state(app_state.clone());

    #[cfg(feature = "developer-mode")]
    let router = {
        warn!("running in developer mode. Rezolus Viewer must be run from within project folder");
        router
            .route_service("/", ServeFile::new("src/viewer/assets/index.html"))
            .nest_service("/lib", ServeDir::new(Path::new("src/viewer/assets/lib")))
            .fallback_service(ServeFile::new("src/viewer/assets/index.html"))
    };

    #[cfg(not(feature = "developer-mode"))]
    let router = {
        router
            .route_service("/", get(index))
            .nest_service("/lib", get(lib))
            .fallback_service(get(index))
    };

    router.layer(
        ServiceBuilder::new()
            .layer(RequestDecompressionLayer::new())
            .layer(CompressionLayer::new())
            .layer(livereload),
    )
}

/// Shared HTML head for standalone pages. Reuses the main viewer
/// stylesheet and applies the saved theme before first paint.
const STANDALONE_HEAD: &str = r#"<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<script>!function(){var t=localStorage.getItem('rezolus-theme');if(t==='light'||t==='dark')document.documentElement.setAttribute('data-theme',t)}()</script>
<link rel="stylesheet" href="/lib/style.css"/>
<style>body{display:flex;align-items:center;justify-content:center;padding:2rem}</style>"#;

async fn about() -> axum::response::Html<String> {
    let version = env!("CARGO_PKG_VERSION");
    axum::response::Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><title>Rezolus — About</title>
{STANDALONE_HEAD}
</head>
<body>
<div class="card">
  <h1>Rezolus</h1>
  <div class="version">v{version}</div>
  <p class="subtitle">High-resolution systems performance telemetry agent.</p>
  <div class="link-row">
    <a href="https://rezolus.com">Website</a>
    <a href="https://github.com/iopsystems/rezolus">GitHub</a>
    <a href="/">Dashboard</a>
  </div>
</div>
</body>
</html>"#
    ))
}

/// Per-section dashboard JSON, generated lazily and memoized.
async fn data(State(state): State<Arc<AppState>>, AxumPath(path): AxumPath<String>) -> Response {
    // Path arrives as "cpu.json" or "service/vllm.json"; LazySectionStore
    // expects "/cpu", "/service/vllm".
    let stem = path.strip_suffix(".json").unwrap_or(&path);
    let route = format!("/{stem}");

    // A section not yet generated reads the recording to find its series,
    // so it is generated in a query slot, under the read lock: `mode` and
    // `sections` are answered meanwhile. Two requests for the same new
    // section can both generate it; the second insert replaces the first.
    // A store replaced meanwhile (a reset or an attach) does not take it.
    let cached = state.sections.read().cached(&route).cloned();
    let value = match cached {
        Some(value) => Some(value),
        None => {
            on_query_slot(&state, move |state| {
                let data = state.baseline_data();
                let (id, value) = {
                    let store = state.sections.read();
                    (store.id(), store.generate(&route, data.as_ref())?)
                };
                state.sections.write().insert(id, &route, value.clone());
                Some(value)
            })
            .await
        }
    };

    let Some(mut value) = value else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // The lazy generator already produces lean bodies, but keep the
    // strip as cheap insurance against accidental re-introduction.
    strip_sections_from_section_payload(&mut value);
    match serde_json::to_string(&value) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(e) => {
            warn!("section response serialization failed for {path}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Drop the navigation `sections` array from a section payload before
/// returning it. Each cached section body embeds the full nav list so
/// that `sections_metadata` can extract it; per-section responses don't
/// need that redundancy.
pub fn strip_sections_from_section_payload(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sections");
    }
}

/// Reports viewer mode (live/file/upload-only, compare attached, etc.).
async fn mode(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let loaded = !state.sections.read().is_empty() || state.is_trimmed_report();
    // The static-site bundle reports "direct" for its own URL input;
    // the binary viewer never reports "direct" — URL loads always go
    // through the local proxy.
    let url_loading = if state.proxy.enabled() {
        "proxy"
    } else {
        "disabled"
    };
    Json(serde_json::json!({
        "live": state.live.load(Ordering::Relaxed),
        // An opened archive file that was not finalized when opened, and is
        // still followed: the page refreshes as in live mode and keeps its
        // file-mode UI.
        "following": state.following(),
        "loaded": loaded,
        "compare_mode": state.captures.experiment_attached(),
        "combined_ab": state.combined_ab(),
        "report": state.is_trimmed_report(),
        "category": state.category_name.read().clone(),
        "url_loading": url_loading,
    }))
}

async fn systeminfo_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Response {
    match state.captures.systeminfo_by_id(p.capture_str()) {
        Some(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn selection_handler(State(state): State<Arc<AppState>>) -> Response {
    match &*state.selection.read() {
        Some(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Navigation list + global capture params; no section bodies.
async fn sections_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "success",
        "data": state.sections_metadata(),
    }))
}

async fn file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Response {
    let body = state
        .captures
        .file_metadata_by_id(p.capture_str())
        .unwrap_or_else(|| "{}".to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// The attached captures, anchor first, in display order:
/// `[{ "id": "baseline", "alias": "redis" }, ...]`.
///
/// The frontend enumerates this to know what an N-way overlay should draw.
/// The browser's `WasmCaptureRegistry::captures()` is the same contract; this
/// is the server side of it.
async fn captures_handler(State(state): State<Arc<AppState>>) -> Response {
    let list: Vec<serde_json::Value> = state
        .captures
        .capture_ids()
        .into_iter()
        .map(|id| {
            let alias = state
                .captures
                .alias_by_id(&id)
                .unwrap_or_else(|| id.clone());
            serde_json::json!({ "id": id, "alias": alias })
        })
        .collect();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string()),
    )
        .into_response()
}

// ── Metric catalog ────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct MetricsParam {
    #[serde(default)]
    capture: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

async fn metrics_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<MetricsParam>,
) -> Response {
    on_query_slot(&state, move |state| {
        let capture_id = p
            .capture
            .as_deref()
            .unwrap_or(capture_registry::BASELINE_ID);
        let Some(data) = state.captures.get_by_id(capture_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = p.source.clone().unwrap_or_else(|| data.source());
        let descriptions = state
            .captures
            .file_metadata_by_id(capture_id)
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(|v| dashboard::metric_catalog::resolve_descriptions(&v, &source))
            .unwrap_or_default();
        let metrics = dashboard::metric_catalog::assemble_catalog(
            data.as_ref(),
            &descriptions,
            p.source.as_deref(),
        );
        let body = dashboard::metric_catalog::MetricsResponse { source, metrics };
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&body).unwrap(),
        )
            .into_response()
    })
    .await
}

// ── Sample timestamps (jitter visualization) ───────────────────────────

#[derive(serde::Serialize)]
struct TimestampsResponse {
    source: String,
    timestamps: Vec<u64>,
}

async fn timestamps_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<MetricsParam>,
) -> Response {
    let capture_id = p
        .capture
        .clone()
        .unwrap_or_else(|| capture_registry::BASELINE_ID.to_string());
    on_query_slot(&state, move |state| {
        let Some(data) = state.captures.get_by_id(capture_id.as_str()) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = p.source.clone().unwrap_or_else(|| data.source());
        let timestamps = data.sample_timestamps();
        Json(TimestampsResponse { source, timestamps }).into_response()
    })
    .await
}

// ── PromQL handlers ───────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct QueryParams {
    query: String,
    time: Option<f64>,
    #[serde(default)]
    capture: Option<String>,
}

#[derive(serde::Deserialize)]
struct RangeQueryParams {
    query: String,
    start: f64,
    end: f64,
    step: f64,
    #[serde(default)]
    capture: Option<String>,
    /// `display` selects the decimated boxplot response (binary). Absent =
    /// today's PromQL-compatible JSON matrix.
    #[serde(default)]
    format: Option<String>,
    /// Point budget per series for display mode. Default 500.
    #[serde(default)]
    points: Option<usize>,
    /// Inner-band quantiles as `"lo,hi"` (e.g. `"0.25,0.75"`). Default IQR.
    #[serde(default)]
    band: Option<String>,
    /// Rate time-alignment mode: `"raw"` for real sample timestamps; absent or
    /// anything else is the default grid-aligned mode.
    #[serde(default)]
    rate_mode: Option<String>,
}

/// Run `f`, which reads the recording, on tokio's blocking pool once one of
/// the viewer's query slots ([`AppState::queries`]) is free. The async
/// workers stay free for requests that do not read the recording, and a slow
/// query holds one slot rather than the server. The slot is held until `f`
/// returns, including after the client disconnects. A panic in `f` releases
/// it and is raised again in the handler.
pub(super) async fn on_query_slot<T, F>(state: &Arc<AppState>, f: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> T + Send + 'static,
{
    let permit = Arc::clone(&state.queries)
        .acquire_owned()
        .await
        .expect("the query semaphore is never closed");
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f(&state)
    })
    .await
    .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
}

/// Run `f` against the resolved capture's data source; on a missing
/// capture, return a `capture_not_found` ApiResponse.
fn run_query<F>(state: &AppState, capture: Option<&str>, f: F) -> Json<ApiResponse<QueryResult>>
where
    F: FnOnce(&dyn metriken_query::MetricsSource) -> Result<QueryResult, QueryError>,
{
    let capture = capture.unwrap_or(capture_registry::BASELINE_ID);
    let Some(data) = state.captures.get_by_id(capture) else {
        return ApiResponse::err(
            format!("capture '{capture}' not attached"),
            "capture_not_found",
        );
    };
    match f(data.as_ref()) {
        Ok(result) => ApiResponse::ok(result),
        Err(e) => ApiResponse::err(e.to_string(), state::promql_error_type(&e)),
    }
}

async fn instant_query(
    Query(params): Query<QueryParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    on_query_slot(&state, move |state| {
        run_query(state, params.capture.as_deref(), |data| {
            data.query(&params.query, params.time)
        })
        .into_response()
    })
    .await
}

async fn range_query(
    Query(params): Query<RangeQueryParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    on_query_slot(&state, move |state| {
        if params.format.as_deref() == Some("display") {
            return range_query_display(state, &params);
        }
        let qopts = metriken_query::QueryOptions::with_rate_mode(display_wire::parse_rate_mode(
            params.rate_mode.as_deref(),
        ));
        run_query(state, params.capture.as_deref(), |data| {
            data.query_range_opts(&params.query, params.start, params.end, params.step, &qopts)
        })
        .into_response()
    })
    .await
}

/// Display-mode range query: decimate to per-bucket boxplots and return the
/// binary columnar wire format. The query + encoding live in the shared
/// `dashboard::display_wire` so the WASM viewer produces byte-identical bodies.
/// Non-`Series` results (scalar/vector) fall back to JSON.
fn range_query_display(state: &AppState, params: &RangeQueryParams) -> Response {
    let capture = params
        .capture
        .as_deref()
        .unwrap_or(capture_registry::BASELINE_ID);
    let Some(data) = state.captures.get_by_id(capture) else {
        return ApiResponse::<serde_json::Value>::err(
            format!("capture '{capture}' not attached"),
            "capture_not_found",
        )
        .into_response();
    };
    match display_wire::display_query(
        data.as_ref(),
        &params.query,
        params.start,
        params.end,
        params.step,
        params.points.unwrap_or(500),
        display_wire::parse_band(params.band.as_deref()),
        display_wire::parse_rate_mode(params.rate_mode.as_deref()),
    ) {
        Ok(display_wire::DisplayWire::Binary(buf)) => {
            ([(header::CONTENT_TYPE, "application/octet-stream")], buf).into_response()
        }
        Ok(display_wire::DisplayWire::Json(result)) => ApiResponse::ok(result).into_response(),
        Err(e) => {
            ApiResponse::<serde_json::Value>::err(e.to_string(), state::promql_error_type(&e))
                .into_response()
        }
    }
}

async fn label_names(State(_state): State<Arc<AppState>>) -> Json<ApiResponse<Vec<String>>> {
    let labels = [
        "__name__",
        "direction",
        "op",
        "state",
        "reason",
        "id",
        "name",
        "sampler",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    ApiResponse::ok(labels)
}

async fn label_values(
    AxumPath(name): AxumPath<String>,
    State(_state): State<Arc<AppState>>,
) -> Json<ApiResponse<Vec<String>>> {
    let values: Vec<String> = match name.as_str() {
        "direction" => ["transmit", "receive", "to", "from"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        "op" => vec!["read".to_string(), "write".to_string()],
        "state" => vec!["user".to_string(), "system".to_string()],
        _ => vec![],
    };
    ApiResponse::ok(values)
}

async fn metadata(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Json<ApiResponse<serde_json::Value>> {
    let capture = p.capture_str();
    let Some(data) = state.captures.get_by_id(capture) else {
        return ApiResponse::err(
            format!("capture '{capture}' not attached"),
            "capture_not_found",
        );
    };
    // time_range is in seconds; metadata endpoint returns seconds too.
    let (min_time, max_time) = data.time_range().unwrap_or((0.0, 0.0));
    // Normalize a degenerate interval (0 for a metadata-less capture,
    // f64::MAX for an empty multi-file reader) to 0.0 so the frontend's
    // `interval || 1` fallback engages instead of producing an absurd step.
    let interval = data.interval();
    let interval = if interval.is_finite() && interval > 0.0 {
        interval
    } else {
        0.0
    };
    let filename = state.captures.filename_by_id(capture);
    let mut meta = serde_json::json!({
        "minTime": min_time,
        "maxTime": max_time,
        "interval": interval,
        "filename": filename,
    });
    // A live recording that has stopped says why, so the page can stop
    // presenting the view as live.
    if capture == capture_registry::BASELINE_ID {
        if let Some(why) = state.live_session.lock().as_ref().and_then(|s| s.stopped()) {
            meta["liveError"] = serde_json::json!(why);
        }
        // Whether a followed archive file is still followed. The page stops
        // refreshing once this is false or absent (the file was finalized or
        // removed, or the baseline was replaced). A file whose writer has
        // stopped stays followed, at a longer wait.
        if state.follow.lock().is_some() {
            meta["following"] = serde_json::json!(state.following());
        }
    }
    if let Some(alias) = state.captures.alias_by_id(capture) {
        meta["alias"] = serde_json::json!(alias);
    }
    if capture == capture_registry::BASELINE_ID {
        if let Some(checksum) = &*state.file_checksum.read() {
            meta["fileChecksum"] = serde_json::json!(checksum);
        }
    }
    ApiResponse::ok(meta)
}

// ── Static asset serving (release builds) ─────────────────────────────

#[cfg(not(feature = "developer-mode"))]
/// A stable ETag for an embedded asset: a hash of its bytes (deterministic —
/// `DefaultHasher::new()` has fixed keys), so it changes exactly when the
/// content does.
#[cfg(not(feature = "developer-mode"))]
fn etag_for(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("\"{:016x}\"", hasher.finish())
}

/// Serve an embedded asset with an ETag + `Cache-Control: no-cache`, honoring
/// `If-None-Match` with a `304` so the browser revalidates on every load and
/// never serves a stale/mixed ES-module set after a rebuild. The assets
/// previously shipped with no validators, so a soft refresh could load old
/// bytes for some modules and new for others.
#[cfg(not(feature = "developer-mode"))]
fn asset_response(bytes: &'static [u8], content_type: &'static str, req: &HeaderMap) -> Response {
    let etag = etag_for(bytes);
    let matched = req
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == etag)
        .unwrap_or(false);
    if matched {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, "no-cache".to_string()),
            ],
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        bytes.to_vec(),
    )
        .into_response()
}

#[cfg(not(feature = "developer-mode"))]
async fn index(headers: HeaderMap) -> Response {
    let Some(asset) = ASSETS.get_file("index.html") else {
        tracing::error!("index.html missing from build");
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found",
        )
            .into_response();
    };
    asset_response(asset.contents(), "text/html", &headers)
}

#[cfg(not(feature = "developer-mode"))]
async fn lib(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    let Some(asset) = ASSETS.get_file(format!("lib{path}")) else {
        tracing::error!("path: {path} does not map to a static resource");
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found",
        )
            .into_response();
    };
    let content_type = match path.rsplit('.').next() {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html",
        Some("json") => "application/json",
        _ => "text/plain",
    };
    asset_response(asset.contents(), content_type, &headers)
}

#[cfg(test)]
mod live_error_tests {
    use super::*;
    use crate::viewer::live::LiveSession;
    use ::dashboard::TemplateRegistry;

    async fn metadata_of(state: Arc<AppState>) -> serde_json::Value {
        let Json(resp) = metadata(State(state), Query(CaptureParam { capture: None })).await;
        serde_json::to_value(resp).unwrap()["data"].clone()
    }

    fn state_with(session: LiveSession) -> Arc<AppState> {
        let state = AppState::new(session.reader(), TemplateRegistry::empty());
        state.install_live(session);
        Arc::new(state)
    }

    /// The page learns that a live recording stopped from the baseline's
    /// metadata, and a recording that is running reports nothing.
    #[tokio::test]
    async fn a_stopped_live_recording_is_reported_in_the_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        crate::dendro_copy::fixtures::recorded(&path, 3, false);

        let stopped = state_with(LiveSession::for_test(
            &path,
            Some("the agent can no longer serve /metrics/stream: HTTP 404".to_string()),
        ));
        assert_eq!(
            metadata_of(stopped).await["liveError"],
            "the agent can no longer serve /metrics/stream: HTTP 404"
        );

        let running = state_with(LiveSession::for_test(&path, None));
        assert!(metadata_of(running).await.get("liveError").is_none());
    }
}

#[cfg(test)]
mod follow_tests {
    use super::*;

    fn view(path: &std::path::Path) -> Arc<AppState> {
        let matches = crate::viewer::command().get_matches_from(["view", path.to_str().unwrap()]);
        let config = crate::viewer::Config::try_from(matches).unwrap();
        Arc::new(crate::viewer::init_file_mode(
            &config,
            path,
            &::dashboard::TemplateRegistry::empty(),
            metriken_query::BufferPool::new(64 << 20),
        ))
    }

    async fn mode_and_metadata(state: Arc<AppState>) -> (serde_json::Value, serde_json::Value) {
        let Json(mode) = mode(State(Arc::clone(&state))).await;
        let Json(meta) = metadata(State(state), Query(CaptureParam { capture: None })).await;
        (mode, serde_json::to_value(meta).unwrap()["data"].clone())
    }

    /// The page learns that a file is followed from `following` in the mode
    /// response, which is distinct from `live`, and from the baseline's
    /// metadata, which says when the follow has ended.
    #[tokio::test]
    async fn a_followed_file_is_reported_apart_from_live_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        crate::dendro_copy::fixtures::recorded(&path, 3, false);
        let (mode, meta) = mode_and_metadata(view(&path)).await;
        assert_eq!(mode["following"], true);
        assert_eq!(mode["live"], false);
        assert_eq!(meta["following"], true);

        let done = dir.path().join("done.dendro");
        crate::dendro_copy::fixtures::recorded(&done, 3, true);
        let (mode, meta) = mode_and_metadata(view(&done)).await;
        assert_eq!(mode["following"], false);
        assert!(meta.get("following").is_none());
    }
}

#[cfg(test)]
mod query_slot_tests {
    use super::*;
    use metriken_query::MetricsSource;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// An empty recording whose reads wait, once inside it, until the test
    /// releases them or 5 s pass. The wait is bounded so that a handler that
    /// blocks the async worker fails the test instead of hanging it.
    struct Gated {
        inner: metriken_query::MemoryStore,
        entered: mpsc::SyncSender<()>,
        release: parking_lot::Mutex<mpsc::Receiver<()>>,
        /// Panic in the next read instead of waiting.
        panic_once: AtomicBool,
    }

    impl Gated {
        fn wait(&self) {
            if self.panic_once.swap(false, Ordering::Relaxed) {
                panic!("a query that panics");
            }
            self.entered.send(()).unwrap();
            let _ = self.release.lock().recv_timeout(Duration::from_secs(5));
        }
    }

    impl MetricsSource for Gated {
        fn query_range_opts(
            &self,
            expr: &str,
            start: f64,
            end: f64,
            step: f64,
            opts: &metriken_query::QueryOptions,
        ) -> Result<QueryResult, QueryError> {
            self.wait();
            self.inner.query_range_opts(expr, start, end, step, opts)
        }
        fn query_range_display_opts(
            &self,
            expr: &str,
            start: f64,
            end: f64,
            step: f64,
            opts: &metriken_query::DisplayOptions,
            qopts: &metriken_query::QueryOptions,
        ) -> Result<metriken_query::DisplayResult, QueryError> {
            self.wait();
            self.inner
                .query_range_display_opts(expr, start, end, step, opts, qopts)
        }
        fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
            self.wait();
            MetricsSource::query(&self.inner, expr, time)
        }
        fn sample_timestamps(&self) -> Vec<u64> {
            self.wait();
            MetricsSource::sample_timestamps(&self.inner)
        }
        fn counter_names(&self) -> Vec<String> {
            self.wait();
            MetricsSource::counter_names(&self.inner)
        }
        fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
            MetricsSource::columns(&self.inner, query)
        }
        fn time_range(&self) -> Option<(f64, f64)> {
            MetricsSource::time_range(&self.inner)
        }
        fn interval(&self) -> f64 {
            MetricsSource::interval(&self.inner)
        }
        fn source(&self) -> String {
            MetricsSource::source(&self.inner)
        }
        fn version(&self) -> String {
            MetricsSource::version(&self.inner)
        }
        fn filename(&self) -> Option<String> {
            MetricsSource::filename(&self.inner)
        }
        fn metadata_get(&self, key: &str) -> Option<String> {
            MetricsSource::metadata_get(&self.inner, key)
        }
        fn file_metadata(&self) -> HashMap<String, String> {
            MetricsSource::file_metadata(&self.inner)
        }
        fn gauge_names(&self) -> Vec<String> {
            MetricsSource::gauge_names(&self.inner)
        }
        fn histogram_names(&self) -> Vec<String> {
            MetricsSource::histogram_names(&self.inner)
        }
        fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
            MetricsSource::counter_labels(&self.inner, name)
        }
        fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
            MetricsSource::gauge_labels(&self.inner, name)
        }
        fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
            MetricsSource::histogram_labels(&self.inner, name)
        }
        fn time_range_ns(&self) -> Option<(u64, u64)> {
            MetricsSource::time_range_ns(&self.inner)
        }
    }

    type Entered = Arc<parking_lot::Mutex<mpsc::Receiver<()>>>;

    /// A one-slot state over a [`Gated`] recording, with the channels that
    /// see a read enter and release it.
    fn gated() -> (Arc<AppState>, Entered, mpsc::Sender<()>, Arc<Gated>) {
        let (entered_tx, entered) = mpsc::sync_channel(4);
        let (release, release_rx) = mpsc::channel();
        let gated = Arc::new(Gated {
            inner: metriken_query::MemoryStore::builder().build(),
            entered: entered_tx,
            release: parking_lot::Mutex::new(release_rx),
            panic_once: AtomicBool::new(false),
        });
        let mut state = AppState::new(
            Arc::clone(&gated) as Arc<dyn MetricsSource>,
            ::dashboard::TemplateRegistry::empty(),
        );
        state.set_query_concurrency(1);
        (
            Arc::new(state),
            Arc::new(parking_lot::Mutex::new(entered)),
            release,
            gated,
        )
    }

    async fn wait_entered(entered: &Entered) {
        let entered = Arc::clone(entered);
        tokio::task::spawn_blocking(move || entered.lock().recv_timeout(Duration::from_secs(5)))
            .await
            .unwrap()
            .expect("a read enters the recording");
    }

    fn range(format: Option<&str>) -> Query<RangeQueryParams> {
        Query(RangeQueryParams {
            query: "up".to_string(),
            start: 0.0,
            end: 10.0,
            step: 1.0,
            capture: None,
            format: format.map(str::to_string),
            points: None,
            band: None,
            rate_mode: None,
        })
    }

    /// Each handler that reads the recording, as a spawned request.
    fn request(kind: usize, state: &Arc<AppState>) -> tokio::task::JoinHandle<Response> {
        let state = State(Arc::clone(state));
        let metrics = || {
            Query(MetricsParam {
                source: None,
                capture: None,
            })
        };
        match kind {
            0 => tokio::spawn(instant_query(
                Query(QueryParams {
                    query: "up".to_string(),
                    time: None,
                    capture: None,
                }),
                state,
            )),
            1 => tokio::spawn(range_query(range(None), state)),
            2 => tokio::spawn(range_query(range(Some("display")), state)),
            3 => tokio::spawn(metrics_handler(state, metrics())),
            _ => tokio::spawn(timestamps_handler(state, metrics())),
        }
    }

    /// A request that is reading the recording holds one query slot, not the
    /// server: on a single async worker, a request that does not read the
    /// recording is answered meanwhile, and with one slot a second request
    /// that reads it waits for the first. For each handler that reads it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_query_holds_a_slot_and_not_the_server() {
        for kind in 0..5 {
            let (state, entered, release, _) = gated();
            let first = request(kind, &state);
            wait_entered(&entered).await;

            // Spawned, so it needs the async worker as a request does.
            let Json(mode) = tokio::time::timeout(
                Duration::from_secs(1),
                tokio::spawn(mode(State(Arc::clone(&state)))),
            )
            .await
            .unwrap_or_else(|_| panic!("handler {kind}: mode is not answered"))
            .unwrap();
            assert!(mode.is_object());

            let second = request(kind, &state);
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                entered.lock().try_recv().is_err(),
                "handler {kind}: the second request waits for the slot"
            );

            release.send(()).unwrap();
            first.await.unwrap();
            wait_entered(&entered).await;
            release.send(()).unwrap();
            second.await.unwrap();
        }
    }

    /// Uploads, attaches and detaches read the recording they load, so each
    /// waits for a query slot while a query holds the only one, and the
    /// async worker still answers a request that does not read it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn uploads_attaches_and_detaches_wait_for_a_slot() {
        use crate::viewer::actions;
        for kind in 0..3 {
            let (state, entered, release, _) = gated();
            let first = request(1, &state);
            wait_entered(&entered).await;

            let st = State(Arc::clone(&state));
            let bytes = axum::body::Bytes::from_static(b"not a recording");
            let action = match kind {
                0 => tokio::spawn(async move {
                    actions::upload_parquet(st, axum::http::HeaderMap::new(), bytes)
                        .await
                        .into_response()
                }),
                1 => tokio::spawn(actions::attach_experiment(
                    st,
                    axum::http::HeaderMap::new(),
                    bytes,
                )),
                _ => tokio::spawn(actions::detach_experiment(st)),
            };
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(!action.is_finished(), "action {kind}: waits for the slot");
            let Json(mode) = tokio::time::timeout(
                Duration::from_secs(1),
                tokio::spawn(mode(State(Arc::clone(&state)))),
            )
            .await
            .unwrap_or_else(|_| panic!("action {kind}: mode is not answered"))
            .unwrap();
            assert!(mode.is_object());

            // Release the query, then every read the action makes.
            release.send(()).unwrap();
            first.await.unwrap();
            let drain = {
                let entered = Arc::clone(&entered);
                std::thread::spawn(move || {
                    while entered
                        .lock()
                        .recv_timeout(Duration::from_millis(500))
                        .is_ok()
                    {
                        let _ = release.send(());
                    }
                })
            };
            tokio::time::timeout(Duration::from_secs(5), action)
                .await
                .unwrap_or_else(|_| panic!("action {kind}: finishes once the slot is free"))
                .unwrap();
            drain.join().unwrap();
        }
    }

    /// A query that panics gives its slot back: with one slot, the next
    /// query runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_query_that_panics_gives_its_slot_back() {
        let (state, entered, release, gated) = gated();
        gated.panic_once.store(true, Ordering::Relaxed);
        assert!(
            request(0, &state).await.is_err(),
            "the panic reaches the handler"
        );
        let next = request(0, &state);
        wait_entered(&entered).await;
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), next)
            .await
            .expect("the next query runs")
            .unwrap();
    }
}
