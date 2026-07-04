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
use http::Uri;
#[cfg(not(feature = "developer-mode"))]
use include_dir::{include_dir, Dir};

#[cfg(feature = "developer-mode")]
use std::path::Path;
#[cfg(feature = "developer-mode")]
use tower_http::services::{ServeDir, ServeFile};

use std::sync::atomic::Ordering;

use metriken_query::{QueryError, QueryResult};

use super::actions;
use super::capture_registry::{self, CaptureId};
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
        .route("/save", get(actions::save_parquet))
        .route("/systeminfo", get(systeminfo_handler))
        .route("/selection", get(selection_handler))
        .route("/sections", get(sections_handler))
        .route("/file_metadata", get(file_metadata_handler))
        .route("/metrics", get(metrics_handler))
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

    let value = {
        let data = state.baseline_data();
        let mut store = state.sections.write();
        store.get_or_generate(&route, data.as_ref()).cloned()
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
    match state.captures.systeminfo(p.capture_id()) {
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
async fn sections_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "success",
        "data": state.sections_metadata(p.capture_id()),
    }))
}

async fn file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Response {
    let body = state
        .captures
        .file_metadata(p.capture_id())
        .unwrap_or_else(|| "{}".to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
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
    let capture_id = CaptureId::parse_opt(p.capture.as_deref());
    let Some(data) = state.captures.get(capture_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let descriptions = state
        .captures
        .file_metadata(capture_id)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("descriptions").and_then(|d| d.as_object()).cloned())
        .unwrap_or_default();
    let source = p.source.clone().unwrap_or_else(|| data.source());
    let metrics = super::metric_catalog::assemble_catalog(
        data.as_ref(),
        &descriptions,
        p.source.as_deref(),
    );
    let body = super::metric_catalog::MetricsResponse { source, metrics };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap(),
    )
        .into_response()
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
}

/// Run `f` against the resolved capture's data source; on a missing
/// capture, return a `capture_not_found` ApiResponse.
fn run_query<F>(state: &AppState, capture: Option<&str>, f: F) -> Json<ApiResponse<QueryResult>>
where
    F: FnOnce(&dyn metriken_query::MetricsSource) -> Result<QueryResult, QueryError>,
{
    let capture = CaptureId::parse_opt(capture);
    let Some(data) = state.captures.get(capture) else {
        return ApiResponse::err(
            format!("capture '{capture:?}' not attached"),
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
) -> Json<ApiResponse<QueryResult>> {
    run_query(&state, params.capture.as_deref(), |data| {
        data.query(&params.query, params.time)
    })
}

async fn range_query(
    Query(params): Query<RangeQueryParams>,
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<QueryResult>> {
    run_query(&state, params.capture.as_deref(), |data| {
        data.query_range(&params.query, params.start, params.end, params.step)
    })
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
    let capture = p.capture_id();
    let Some(data) = state.captures.get(capture) else {
        return ApiResponse::err(
            format!("capture {capture:?} not attached"),
            "capture_not_found",
        );
    };
    // time_range is in seconds; metadata endpoint returns seconds too.
    let (min_time, max_time) = data.time_range().unwrap_or((0.0, 0.0));
    let filename = state.captures.filename(capture);
    let mut meta = serde_json::json!({
        "minTime": min_time,
        "maxTime": max_time,
        "filename": filename,
    });
    if let Some(alias) = state.captures.alias(capture) {
        meta["alias"] = serde_json::json!(alias);
    }
    if matches!(capture, capture_registry::CaptureId::Baseline) {
        if let Some(checksum) = &*state.file_checksum.read() {
            meta["fileChecksum"] = serde_json::json!(checksum);
        }
    }
    ApiResponse::ok(meta)
}

// ── Static asset serving (release builds) ─────────────────────────────

#[cfg(not(feature = "developer-mode"))]
async fn index() -> impl IntoResponse {
    if let Some(asset) = ASSETS.get_file("index.html") {
        let body = asset.contents_utf8().unwrap();
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
            body.to_string(),
        )
    } else {
        tracing::error!("index.html missing from build");
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found".to_string(),
        )
    }
}

#[cfg(not(feature = "developer-mode"))]
async fn lib(uri: Uri) -> impl IntoResponse {
    let path = uri.path();
    let Some(asset) = ASSETS.get_file(format!("lib{path}")) else {
        tracing::error!("path: {path} does not map to a static resource");
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found".to_string(),
        );
    };
    let body = asset.contents_utf8().unwrap();
    let content_type = match path.rsplit('.').next() {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html",
        Some("json") => "application/json",
        _ => "text/plain",
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        body.to_string(),
    )
}
