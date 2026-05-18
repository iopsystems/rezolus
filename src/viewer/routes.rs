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

use std::path::PathBuf;

use super::actions;
use super::capture_registry::{self, CaptureId};
use super::state::{ApiResponse, AppState, CaptureParam};
use ::dashboard;

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
        .route("/save", get(actions::save_parquet))
        .route("/systeminfo", get(systeminfo_handler))
        .route("/selection", get(selection_handler))
        .route("/sections", get(sections_handler))
        .route("/file_metadata", get(file_metadata_handler))
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
        .route(
            "/save_with_selection",
            axum::routing::post(actions::save_with_selection),
        )
        .route("/load_url", axum::routing::post(actions::load_url));

    // Live-only routes — present only when the live-agent path is
    // compiled in. SQL-only builds reject `/api/v1/connect` and
    // `/api/v1/reset` with the framework's default 404.
    #[cfg(feature = "live-mode")]
    let api_routes = api_routes
        .route("/reset", axum::routing::post(actions::reset_tsdb))
        .route("/connect", axum::routing::post(actions::connect_agent));

    let api_routes = api_routes.layer(axum::middleware::map_response(
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

    let value = state.with_baseline_data(|data| {
        let mut store = state.sections.write();
        store.get_or_generate(&route, data).cloned()
    });
    let value = value.flatten();

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

// ── SQL handlers ──────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct QueryParams {
    /// SQL string. Dashboard plots ship one of these in
    /// `plot.sql_query`; the frontend forwards it verbatim. Schema
    /// contract: `t` (DOUBLE seconds), `v` (numeric), zero or more
    /// label columns. See `crates/dashboard/src/sql.rs` for emitters.
    query: String,
    /// Unused on the SQL path (queries are self-contained). Accepted
    /// for URL-shape compatibility with the legacy PromQL endpoint.
    #[allow(dead_code)]
    time: Option<f64>,
    #[serde(default)]
    capture: Option<String>,
}

#[derive(serde::Deserialize)]
struct RangeQueryParams {
    query: String,
    /// Unused — frontend embeds time bounds in the SQL body (`_src`
    /// CTE on the WASM side, plain `WHERE timestamp BETWEEN …` here
    /// if the caller wants it). Accepted for URL-shape compatibility.
    #[allow(dead_code)]
    start: f64,
    #[allow(dead_code)]
    end: f64,
    #[allow(dead_code)]
    step: f64,
    #[serde(default)]
    capture: Option<String>,
}

/// Resolve the parquet path for the requested capture. Returns `None`
/// in live-agent mode (no parquet on disk) or when the experiment
/// slot is empty — caller surfaces those as 503 / capture_not_found.
fn parquet_path_for(state: &AppState, capture: CaptureId) -> Option<PathBuf> {
    match capture {
        CaptureId::Baseline => state.parquet_path.read().clone(),
        CaptureId::Experiment => state
            .experiment_parquet_path
            .read()
            .clone()
            .or_else(|| state.cli_experiment_path.read().clone()),
    }
}

/// Run `sql` against the resolved capture's parquet through the
/// shared `DuckDbBackend`. Returns a Prometheus matrix-shape JSON
/// envelope (success + matrix or success + empty matrix). Errors are
/// surfaced as `ApiResponse::err` with a `sql_error` type tag.
///
/// The DuckDB call and the Arrow→JSON projection are CPU/blocking
/// work, so we offload to `spawn_blocking`. Calling synchronously
/// from an axum handler holds a tokio worker thread for the full
/// query duration — with 20+ chart queries firing in parallel on
/// section load, that starves the runtime and serializes them onto
/// the small worker pool regardless of the DuckDB backend's
/// connection-pool size. spawn_blocking off-loads to tokio's
/// blocking-task pool (default 512 threads), so all `pool_size`
/// DuckDB slots can run concurrently and async handlers (static
/// assets, sections nav, etc.) stay responsive.
async fn run_sql(state: &Arc<AppState>, capture: Option<&str>, sql: String) -> Response {
    let capture = CaptureId::parse_opt(capture);
    let Some(path) = parquet_path_for(state, capture) else {
        // Live-agent mode (no parquet) or unattached experiment.
        // Live mode is a known carve-out during the SQL migration —
        // see plan stages 3-9. Surfaced as capture_not_found so users
        // notice early.
        return ApiResponse::<serde_json::Value>::err(
            format!("capture '{capture:?}' has no parquet (live mode or unattached)"),
            "capture_not_found",
        )
        .into_response();
    };
    let data_source = path.to_string_lossy().to_string();
    let backend = state.sql_backend.clone();
    let outcome = tokio::task::spawn_blocking(move || match backend.run_sql(&sql, &data_source) {
        Ok(batches) => Ok(prom_matrix::arrow_to_prom_matrix(&batches)),
        Err(e) => Err(e.to_string()),
    })
    .await;
    let outcome = match outcome {
        Ok(o) => o,
        Err(join_err) => {
            return ApiResponse::<serde_json::Value>::err(
                format!("query task panicked: {join_err}"),
                "sql_error",
            )
            .into_response();
        }
    };
    match outcome {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(msg) => {
            // "No matching columns" is DuckDB's binder error when a
            // `COLUMNS('regex')` spread matches zero columns — which
            // is "this metric is not in this parquet", not a SQL
            // bug. Translate it to an empty matrix so the frontend
            // renders the chart as "no data" instead of as an error.
            // Mirrors the legacy `Tsdb`+PromQL behaviour where an
            // unknown metric simply returned an empty result set.
            if msg.contains("No matching columns")
                || msg.contains("not found in FROM clause")
            {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    prom_matrix::EMPTY_PROM_MATRIX.to_string(),
                )
                    .into_response();
            }
            ApiResponse::<serde_json::Value>::err(msg, "sql_error").into_response()
        }
    }
}

async fn instant_query(
    Query(params): Query<QueryParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_sql(&state, params.capture.as_deref(), params.query).await
}

async fn range_query(
    Query(params): Query<RangeQueryParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    run_sql(&state, params.capture.as_deref(), params.query).await
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
    // Pull (min_time_ns, max_time_ns, filename) from whichever
    // backend the slot is using; convert to milliseconds for the
    // legacy JSON shape (QueryEngine::get_time_range returned ms).
    let scalar = read_capture_scalar_meta(&state, capture);
    let Some((min_time_ns, max_time_ns, filename)) = scalar else {
        return ApiResponse::err(
            format!("capture {capture:?} not attached"),
            "capture_not_found",
        );
    };
    let mut meta = serde_json::json!({
        "minTime": min_time_ns / 1_000_000,
        "maxTime": max_time_ns / 1_000_000,
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

/// Shared helper for the metadata endpoint: pull (min_time_ns,
/// max_time_ns, filename) from a capture slot regardless of whether
/// it's backed by a Tsdb or a SqlCapture. Returns `None` for an
/// unattached experiment slot.
fn read_capture_scalar_meta(
    state: &AppState,
    capture: CaptureId,
) -> Option<(u64, u64, String)> {
    use dashboard::DashboardData;
    let read = |data: &dyn DashboardData| -> (u64, u64, String) {
        let (lo, hi) = data.time_range().unwrap_or((0, 0));
        (lo, hi, data.filename().to_string())
    };
    if let Some(handle) = state.captures.get_sql(capture) {
        return Some(read(&*handle.read()));
    }
    #[cfg(feature = "live-mode")]
    if let Some(handle) = state.captures.get(capture) {
        return Some(read(&*handle.read()));
    }
    None
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
