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
        .route("/section_status", get(section_status))
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

/// Per-section status: total plot specs the dashboard generator
/// produced + count whose SQL query actually returns data against
/// the current capture. Drives the sidebar's "gray out empty
/// sections" affordance on page load, before the user has clicked
/// through to each section.
///
/// Server-driven so the frontend gets the whole picture in one
/// request instead of triggering ~13 `loadSection` calls (each
/// firing ~20 queries) just to populate sidebar status.
async fn section_status(
    State(state): State<Arc<AppState>>,
    Query(p): Query<CaptureParam>,
) -> Json<ApiResponse<serde_json::Value>> {
    let capture = p.capture_id();
    let Some(data_source) = data_source_for(&state, capture) else {
        return ApiResponse::err("capture not attached", "capture_not_found");
    };

    let state_clone = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Clone the section list so we don't hold the sections-read
        // lock across the per-section generation calls (each grabs
        // the write lock).
        let sections: Vec<dashboard::Section> = state_clone
            .sections
            .read()
            .sections()
            .to_vec();

        let mut status = serde_json::Map::new();
        for section in &sections {
            // Skip non-data sections — they have no dashboard JSON to
            // generate (the Query Explorer is a UI, not a plot page).
            // Routes seen here: /overview, /cpu, /service/vllm, …
            if !section.route.starts_with('/') {
                continue;
            }
            // Get-or-generate the section body. Cached after first
            // call, so /api/v1/section_status pays the dashboard-
            // generation cost once and subsequent navigations to
            // each section hit the cache.
            let body = state_clone
                .with_baseline_data(|data| {
                    let mut store = state_clone.sections.write();
                    store.get_or_generate(&section.route, data).cloned()
                })
                .flatten();
            let Some(body) = body else { continue };

            let counts = count_section_plots(&body, &state_clone.sql_backend, &data_source);

            // Sidebar keys responses by the route without the leading slash
            // (matches `sectionResponseCache` indexing in `app.js`).
            let key = section.route.trim_start_matches('/').to_string();
            status.insert(
                key,
                serde_json::json!({
                    "total": counts.total,
                    "withData": counts.with_data,
                }),
            );
        }
        serde_json::Value::Object(status)
    })
    .await;

    match result {
        Ok(v) => ApiResponse::ok(v),
        Err(e) => ApiResponse::err(format!("task panicked: {e}"), "task_error"),
    }
}

struct SectionCounts {
    /// Plots the client-side renderer would keep — mirrors
    /// `data.js::processDashboardData`. This is the number the
    /// sidebar shows as `(N)` and uses for the gray-out threshold.
    total: u32,
    /// Subset of `total` whose SQL query actually returned data.
    /// Exposed for future affordances; the sidebar currently treats
    /// `total === 0` as "gray me out" since the renderer would emit
    /// no chart wrappers at all in that case.
    with_data: u32,
}

/// Server-side equivalent of `data.js::processDashboardData`'s
/// plot-stripping pass. Walks a section's `groups → subgroups → plots`
/// and counts which plots would survive on the client.
///
/// A plot is "kept" when any of these is true:
///   - Carries `__SELECTED_CGROUPS__` in its SQL or PromQL (deferred
///     cgroup plot — the cgroup_selector refetches it on demand).
///   - Carries no PromQL query at all (legacy template plot that
///     would render as static markup).
///   - PromQL-only KPI with no SQL on the SQL backend (would render
///     a `_unavailable` placeholder card).
///   - SQL query returned at least one non-empty Arrow batch.
///
/// Errors are swallowed and treated as "no data" — same end
/// behaviour as the client's `processDashboardData`, which pushes
/// failures into `unavailable_charts` (a separate notes list that
/// the sidebar doesn't count).
fn count_section_plots(
    body: &serde_json::Value,
    backend: &metriken_query_sql::DuckDbBackend,
    data_source: &str,
) -> SectionCounts {
    let mut total = 0u32;
    let mut with_data = 0u32;
    let Some(groups) = body.get("groups").and_then(|v| v.as_array()) else {
        return SectionCounts { total: 0, with_data: 0 };
    };
    // Mirror the JS shape: groups → (subgroups → plots) | direct plots.
    for group in groups {
        let subgroups = group.get("subgroups").and_then(|v| v.as_array());
        let direct_plots = group.get("plots").and_then(|v| v.as_array());
        let plot_iter = subgroups
            .into_iter()
            .flat_map(|sgs| sgs.iter())
            .filter_map(|sg| sg.get("plots").and_then(|v| v.as_array()))
            .chain(direct_plots.into_iter())
            .flatten();
        for plot in plot_iter {
            let promql = plot
                .get("promql_query")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let sql = plot
                .get("sql_query")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Cgroup deferred: keep without running. The selector
            // splices in the picked cgroups before firing the query
            // — running it as-is here would either bind-error or
            // return empty.
            if sql.contains("__SELECTED_CGROUPS__")
                || promql.contains("__SELECTED_CGROUPS__")
            {
                total += 1;
                continue;
            }
            // No queries at all: a static / template plot.
            if promql.is_empty() && sql.is_empty() {
                total += 1;
                continue;
            }
            // PromQL-only on the SQL backend: would render as a
            // `_unavailable` placeholder (May-18 commit `6054fe2`),
            // so the section is content-bearing even if not
            // chart-rendering.
            if !promql.is_empty() && sql.is_empty() {
                total += 1;
                continue;
            }
            // SQL is present — run it. Counts toward `total` only if
            // there's actual data.
            if let Ok(batches) = backend.run_sql(sql, data_source) {
                if batches.iter().any(|b| b.num_rows() > 0) {
                    total += 1;
                    with_data += 1;
                }
            }
        }
    }
    SectionCounts { total, with_data }
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
/// slot is empty.
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

/// Resolve the SQL backend's `data_source` string for the requested
/// capture. For file / upload / A-B captures this is the parquet path;
/// for live captures it's the live-source key registered with
/// [`DuckDbBackend::create_live_source`]. Returns `None` for an
/// unattached experiment slot or an upload-only viewer before its
/// first upload.
fn data_source_for(state: &AppState, capture: CaptureId) -> Option<String> {
    // Live mode: only the baseline slot is ever live (live captures
    // are single-source by construction). When `live_source` is `Some`
    // for the baseline, route SQL queries there.
    #[cfg(feature = "live-mode")]
    if matches!(capture, CaptureId::Baseline)
        && state.live_source.read().is_some()
    {
        return Some(super::state::LIVE_BASELINE_DATA_SOURCE.to_string());
    }
    parquet_path_for(state, capture).map(|p| p.to_string_lossy().into_owned())
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
    let Some(data_source) = data_source_for(state, capture) else {
        // Unattached experiment slot, or upload-only viewer pre-upload.
        return ApiResponse::<serde_json::Value>::err(
            format!("capture '{capture:?}' not attached"),
            "capture_not_found",
        )
        .into_response();
    };
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

#[cfg(all(test, feature = "live-mode"))]
mod live_route_tests {
    //! Routing + live source end-to-end. The L3 layer of the
    //! regression net: build an `AppState` with a populated
    //! `LiveSource`, exercise `data_source_for` + the backend's
    //! routing, and assert query results flow back through.
    //!
    //! Bypasses axum (which would add ~100 lines of router-mounting
    //! boilerplate) and calls the routing logic directly. The HTTP
    //! plumbing — `instant_query` / `range_query` handlers — is
    //! one-line wrappers that just forward into `run_sql`, and is
    //! exercised end-to-end by `viewer_smoke.sh` / the chromium smoke.
    //! What needs *specific* testing is the live-vs-parquet dispatch,
    //! which lives in `data_source_for` and the backend's
    //! `live_sources` map.

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::super::state::{AppState, LIVE_BASELINE_DATA_SOURCE};
    use super::super::tsdb::Tsdb;
    use super::{data_source_for, CaptureId};
    use ::dashboard::TemplateRegistry;
    use metriken_query_sql::{LiveColumn, LiveColumnKind, LiveValue};

    /// Build an `AppState` in live mode with the sql_backend's
    /// live-source slot populated; returns the appender so the test
    /// can drive snapshots.
    fn live_state() -> (Arc<AppState>, Arc<metriken_query_sql::LiveSource>) {
        let state = AppState::new(Tsdb::default(), TemplateRegistry::empty());
        let live = state
            .sql_backend
            .create_live_source(LIVE_BASELINE_DATA_SOURCE, "rezolus", 1000)
            .expect("create_live_source");
        *state.live_source.write() = Some(live.clone());
        state
            .live
            .store(true, std::sync::atomic::Ordering::Relaxed);
        (Arc::new(state), live)
    }

    fn col(physical: &str, metric: &str, kind: LiveColumnKind) -> LiveColumn {
        LiveColumn {
            physical: physical.into(),
            metric: metric.into(),
            kind,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn data_source_for_live_baseline_returns_live_key() {
        let (state, _live) = live_state();
        let data_source = data_source_for(&state, CaptureId::Baseline)
            .expect("baseline should resolve");
        assert_eq!(data_source, LIVE_BASELINE_DATA_SOURCE);
    }

    #[test]
    fn data_source_for_live_experiment_is_none_when_unattached() {
        // Live mode pins baseline only; experiment slot returns None
        // (no parquet, no live source). Caller surfaces as
        // capture_not_found.
        let (state, _live) = live_state();
        assert!(data_source_for(&state, CaptureId::Experiment).is_none());
    }

    #[test]
    fn data_source_for_file_mode_returns_parquet_path() {
        // No live source → falls through to parquet_path. Pin that
        // the live carve-out doesn't shadow file-mode captures.
        let state = AppState::new_empty(TemplateRegistry::empty());
        *state.parquet_path.write() = Some(std::path::PathBuf::from("/tmp/test.parquet"));
        let state = Arc::new(state);
        let data_source = data_source_for(&state, CaptureId::Baseline)
            .expect("file mode should resolve");
        assert_eq!(data_source, "/tmp/test.parquet");
    }

    #[test]
    fn backend_routes_live_data_source_to_live_source() {
        // The contract: state.sql_backend.run_sql(sql, LIVE_BASELINE_DATA_SOURCE)
        // dispatches to the LiveSource, not the parquet pool. Pin by
        // appending a snapshot and observing the row through run_sql.
        let (state, live) = live_state();

        let c = col("requests", "requests", LiveColumnKind::Counter);
        live.append(
            1_000_000_000,
            None,
            &[(c.clone(), LiveValue::Counter(42))],
        )
        .expect("append");
        live.append(
            2_000_000_000,
            None,
            &[(c, LiveValue::Counter(100))],
        )
        .expect("append");

        let batches = state
            .sql_backend
            .run_sql(
                "SELECT timestamp, requests FROM _src ORDER BY timestamp",
                LIVE_BASELINE_DATA_SOURCE,
            )
            .expect("run_sql via backend");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 2);
    }

    #[test]
    fn metadata_time_range_advances_as_snapshots_are_appended() {
        // The metadata handler reads time range from
        // `read_capture_scalar_meta`, which uses the Tsdb-backed
        // DashboardData impl during the transition. Even without
        // wiring Tsdb here, confirm the live source itself reports
        // advancing time bounds — pinning the contract the eventual
        // metadata-from-LiveSource migration will rely on.
        let (_state, live) = live_state();
        assert_eq!(live.time_range_ns().expect("range"), None);

        let c = col("requests", "requests", LiveColumnKind::Counter);
        live.append(1_000_000_000, None, &[(c.clone(), LiveValue::Counter(1))])
            .expect("append 1");
        let (lo1, hi1) = live.time_range_ns().expect("range").unwrap();
        assert_eq!(lo1, 1_000_000_000);
        assert_eq!(hi1, 1_000_000_000);

        live.append(3_000_000_000, None, &[(c, LiveValue::Counter(3))])
            .expect("append 2");
        let (lo2, hi2) = live.time_range_ns().expect("range").unwrap();
        assert_eq!(lo2, 1_000_000_000);
        assert_eq!(hi2, 3_000_000_000);
    }
}
