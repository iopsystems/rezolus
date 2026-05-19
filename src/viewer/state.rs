//! Viewer server state and shared types.
//!
//! `AppState` is the single Arc-shared handle threaded into every axum
//! handler. The ancillary types (`LazySectionStore`, `ProxyState`,
//! `ApiResponse`, `CaptureParam`) live here too because they're the
//! HTTP-level scaffolding the handlers all touch.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use reqwest::Client;
use tracing::error;

use super::capture_registry::{CaptureBackend, CaptureId, CaptureRegistry};
use super::live_capture::LiveCapture;
use super::proxy_allow;
use super::sql_capture::SqlCapture;
use ::dashboard::{self, TemplateRegistry};
use metriken_query_sql::{DuckDbBackend, LiveSource};

/// Caches the navigation list (via the owned `DashboardContext`) and
/// memoizes per-section JSON bodies. `/api/v1/sections` reads the nav
/// without materializing any body; `/data/<section>.json` generates a
/// body on first request and serves the cached value thereafter.
pub struct LazySectionStore {
    context: dashboard::dashboard::DashboardContext,
    cached_bodies: HashMap<String, serde_json::Value>,
}

impl LazySectionStore {
    pub fn new(context: dashboard::dashboard::DashboardContext) -> Self {
        Self {
            context,
            cached_bodies: HashMap::new(),
        }
    }

    pub fn sections(&self) -> &[dashboard::Section] {
        &self.context.sections
    }

    /// True when no context has been loaded — used by the `mode` endpoint.
    pub fn is_empty(&self) -> bool {
        self.context.sections.is_empty()
    }

    pub fn context(&self) -> &dashboard::dashboard::DashboardContext {
        &self.context
    }

    /// Generate (or return the cached body for) `route` (`/cpu`,
    /// `/service/vllm`, …). Returns `None` when the route is unknown or
    /// the section has no data. Applies `context.filesize` uniformly.
    pub fn get_or_generate(
        &mut self,
        route: &str,
        data: &dyn dashboard::DashboardData,
    ) -> Option<&serde_json::Value> {
        let key = format!("{}.json", &route[1..]);
        if !self.cached_bodies.contains_key(&key) {
            let mut view = dashboard::dashboard::generate_section(data, route, &self.context)?;
            if let Some(size) = self.context.filesize {
                view.set_filesize(size);
            }
            let value = serde_json::to_value(&view).ok()?;
            self.cached_bodies.insert(key.clone(), value);
        }
        self.cached_bodies.get(&key)
    }
}

impl Default for LazySectionStore {
    fn default() -> Self {
        Self::new(dashboard::dashboard::DashboardContext::default())
    }
}

/// Optional URL-fetch proxy. Disabled (no client) unless the CLI was
/// invoked with `--proxy-allow`/`--proxy-allow-any`.
#[derive(Default)]
pub struct ProxyState {
    pub allow: proxy_allow::Allowlist,
    pub client: Option<Client>,
}

impl ProxyState {
    pub fn enabled(&self) -> bool {
        !self.allow.is_empty() && self.client.is_some()
    }
}

/// `data_source` key registered with [`DuckDbBackend::create_live_source`]
/// for the baseline live capture. Routes layer passes this string to
/// `sql_backend.run_sql(...)` for live captures so the backend dispatches
/// to the right `LiveSource`. Kept short + namespaced so it can't
/// collide with any real parquet path.
pub const LIVE_BASELINE_DATA_SOURCE: &str = "live:baseline";

pub struct AppState {
    pub sections: RwLock<LazySectionStore>,
    /// Per-capture data store + metadata (`SqlCapture` for file/upload/A-B,
    /// `LiveCapture` for live agent). Single-capture callers always target
    /// `CaptureId::Baseline`; the experiment slot is empty unless a
    /// compare-mode hand-off has attached one.
    pub captures: Arc<CaptureRegistry>,
    pub templates: TemplateRegistry,
    /// Raw msgpack snapshot bytes for parquet export (live mode only).
    pub snapshots: Arc<Mutex<VecDeque<Vec<u8>>>>,
    pub live: AtomicBool,
    /// Original parquet file path (file mode only).
    pub parquet_path: RwLock<Option<PathBuf>>,
    /// Temp parquet path for the HTTP-attached experiment capture.
    /// Owned by the attach handler — deleted on detach. The CLI startup
    /// path uses `cli_experiment_path` instead so detach never touches
    /// the user's own file.
    pub experiment_parquet_path: RwLock<Option<PathBuf>>,
    /// User-supplied experiment parquet path from the CLI. Read-only —
    /// never deleted on detach. Kept separate from
    /// `experiment_parquet_path` so `regenerate_dashboards` can find
    /// the experiment metadata without risking the user's file.
    pub cli_experiment_path: RwLock<Option<PathBuf>>,
    /// Active category template name (when `--category` was supplied).
    pub category_name: RwLock<Option<String>>,
    /// Serialized selection JSON from parquet metadata.
    pub selection: RwLock<Option<String>>,
    /// SHA-256 hex digest of the source parquet file (file mode only).
    pub file_checksum: RwLock<Option<String>>,
    pub proxy: ProxyState,
    /// Set during init_file_mode when the input was a `*.parquet.ab.tar`
    /// archive. Carries the manifest extracted from the tarball; the
    /// presence of `Some` is what `/api/v1/mode` exposes as
    /// `combined_ab: true` so the frontend can pick UX appropriate for
    /// a single-artifact compare.
    pub combined_ab_marker: RwLock<Option<crate::parquet_metadata::AbContainers>>,
    /// Footer `KEY_REPORT` value cached at init. `Some("trimmed")`
    /// flips the viewer into report mode (empty section list, frontend
    /// defaults to `/report`).
    pub trimmed_report_marker: RwLock<Option<String>>,
    /// Shared DuckDB-backed SQL execution backend. One per process,
    /// cloned (`Arc`) into every handler that needs it. The backend
    /// holds a per-parquet-path connection pool so the first request
    /// for a given capture pays the cold-start cost and subsequent
    /// requests hit the warm pool. Used by the SQL query handlers and
    /// `SqlCapture` loading.
    pub sql_backend: Arc<DuckDbBackend>,
    /// Live-mode in-memory DuckDB data source. `Some` when the viewer
    /// was started against a live agent; `None` for file / upload /
    /// A-B captures. The ingest loop calls `live_source.append(...)`
    /// on every poll; query handlers route to it via the registered
    /// data-source key (see [`LIVE_BASELINE_DATA_SOURCE`]).
    pub live_source: RwLock<Option<Arc<LiveSource>>>,
    /// Serializes baseline-ingest operations
    /// (`actions::ingest_baseline_from_path`). Without this, two
    /// concurrent `/api/v1/upload` calls can interleave their
    /// post-swap metadata stamping — registry baseline ends up as
    /// upload B's capture while `state.parquet_path` points at A's
    /// path. Held across the parquet load + replace_baseline_with_sql
    /// + the cluster of `state.*.write()` updates so the world sees
    /// a single coherent snapshot per upload.
    pub upload_mutex: Mutex<()>,
}

impl AppState {
    /// Live-agent init constructor. Baseline carries a `LiveCapture`
    /// wrapping the shared `LiveSource` (also registered on the
    /// supplied `DuckDbBackend`'s `live_sources` map under
    /// `LIVE_BASELINE_DATA_SOURCE`).
    pub fn new_live(
        live: LiveCapture,
        backend: Arc<DuckDbBackend>,
        templates: TemplateRegistry,
    ) -> Self {
        let inner = CaptureBackend::Live(Arc::new(RwLock::new(live)));
        let mut state = Self::with_registry(CaptureRegistry::new(Some(inner)), templates);
        state.sql_backend = backend;
        state
    }

    /// Upload-only init constructor. The registry starts with no
    /// baseline; the first `/api/v1/upload` or `/api/v1/load_url`
    /// installs one via `replace_baseline_with_sql`.
    pub fn new_empty(templates: TemplateRegistry) -> Self {
        Self::with_registry(CaptureRegistry::new(None), templates)
    }

    /// File / A-B / CLI-experiment init constructor. Wires the
    /// registry's baseline slot to a SqlCapture. The caller must have
    /// constructed the SqlCapture via the same `DuckDbBackend` that
    /// will be stored on `state.sql_backend` — otherwise the pool
    /// warmed by `SqlCapture::open` is unreachable from subsequent
    /// query handlers.
    pub fn new_sql(
        capture: SqlCapture,
        backend: Arc<DuckDbBackend>,
        templates: TemplateRegistry,
    ) -> Self {
        let inner = CaptureBackend::Sql(Arc::new(RwLock::new(capture)));
        let mut state = Self::with_registry(CaptureRegistry::new(Some(inner)), templates);
        state.sql_backend = backend;
        state
    }

    fn with_registry(captures: CaptureRegistry, templates: TemplateRegistry) -> Self {
        Self {
            sections: Default::default(),
            captures: Arc::new(captures),
            templates,
            snapshots: Arc::new(Mutex::new(VecDeque::new())),
            live: AtomicBool::new(false),
            parquet_path: RwLock::new(None),
            experiment_parquet_path: RwLock::new(None),
            cli_experiment_path: RwLock::new(None),
            category_name: RwLock::new(None),
            selection: RwLock::new(None),
            file_checksum: RwLock::new(None),
            proxy: ProxyState::default(),
            combined_ab_marker: RwLock::new(None),
            trimmed_report_marker: RwLock::new(None),
            sql_backend: Arc::new(DuckDbBackend::new()),
            live_source: RwLock::new(None),
            upload_mutex: Mutex::new(()),
        }
    }

    /// Enable the URL proxy with the given hostname allowlist. Builds a
    /// dedicated reqwest client so proxy traffic is isolated from the
    /// live-mode scrape client. No-op when the allowlist is empty.
    pub fn set_proxy(&mut self, allow: proxy_allow::Allowlist) {
        if allow.is_empty() {
            return;
        }
        match Client::builder().build() {
            Ok(client) => {
                self.proxy = ProxyState {
                    allow,
                    client: Some(client),
                };
            }
            Err(e) => error!("failed to build proxy http client: {e}"),
        }
    }

    /// Run `f` against the baseline slot's `DashboardData` view —
    /// either a `SqlCapture` (file / upload / A-B) or a `LiveCapture`
    /// (live agent). Wraps the read-guard lifetime so handlers don't
    /// have to branch on backend type for the common metadata reads.
    /// Returns `None` when no baseline is loaded yet (upload-only
    /// mode pre-upload).
    pub fn with_baseline_data<R>(
        &self,
        f: impl FnOnce(&dyn dashboard::DashboardData) -> R,
    ) -> Option<R> {
        if let Some(handle) = self.captures.get_sql(CaptureId::Baseline) {
            return Some(f(&*handle.read()));
        }
        if let Some(handle) = self.captures.get_live(CaptureId::Baseline) {
            return Some(f(&*handle.read()));
        }
        None
    }

    /// True when the input artifact was a combined-A/B tarball
    /// (extracted at startup into two per-side `SqlCapture`s). The
    /// frontend uses this to distinguish a single-file compare from a
    /// two-file compare in download / save flows.
    pub fn combined_ab(&self) -> bool {
        self.combined_ab_marker.read().is_some()
    }

    /// True when the loaded parquet carries `KEY_REPORT` — see
    /// [`AppState::trimmed_report_marker`] for what that flips.
    pub fn is_trimmed_report(&self) -> bool {
        self.trimmed_report_marker.read().is_some()
    }

    /// Build the navigation + global params payload for `/api/v1/sections`.
    /// When no context has been loaded yet (live mode pre-refresh,
    /// upload-only mode pre-upload) returns a minimal payload with empty
    /// sections and zeroed numerics. The `_capture` argument is advisory:
    /// the same nav list applies to both baseline and experiment.
    pub fn sections_metadata(&self, _capture: CaptureId) -> serde_json::Value {
        let store = self.sections.read();
        let sections_array: Vec<serde_json::Value> = store
            .sections()
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or_default())
            .collect();
        let filesize = store.context().filesize.unwrap_or(0);
        drop(store);

        // Pull scalar metadata + series count from whichever backend
        // the baseline slot is using (`SqlCapture` or `LiveCapture`);
        // the `DashboardData` trait abstracts both. Default to zeros
        // when no baseline is loaded yet (upload-only mode pre-upload).
        let (interval, source, version, filename, start_time, end_time, num_series) = self
            .with_baseline_data(|data| {
                let (start_time, end_time) = data
                    .time_range()
                    .map(|(min, max)| (min / 1_000_000, max / 1_000_000))
                    .unwrap_or((0, 0));
                let mut num_series = 0usize;
                for name in data.counter_names() {
                    num_series += data.counter_label_count(name);
                }
                for name in data.gauge_names() {
                    num_series += data.gauge_label_count(name);
                }
                for name in data.histogram_names() {
                    num_series += data.histogram_label_count(name);
                }
                (
                    data.interval(),
                    data.source().to_string(),
                    data.version().to_string(),
                    data.filename().to_string(),
                    start_time,
                    end_time,
                    num_series,
                )
            })
            .unwrap_or((0.0, String::new(), String::new(), String::new(), 0, 0, 0));

        build_sections_metadata_payload(
            sections_array,
            &source,
            &version,
            &filename,
            interval,
            filesize,
            start_time,
            end_time,
            num_series,
        )
    }
}

/// Pure helper for `/api/v1/sections` — kept separate so the JSON shape
/// is trivially unit-testable.
#[allow(clippy::too_many_arguments)]
pub fn build_sections_metadata_payload(
    sections: Vec<serde_json::Value>,
    source: &str,
    version: &str,
    filename: &str,
    interval: f64,
    filesize: u64,
    start_time: u64,
    end_time: u64,
    num_series: usize,
) -> serde_json::Value {
    serde_json::json!({
        "sections": sections,
        "source": source,
        "version": version,
        "filename": filename,
        "interval": interval,
        "filesize": filesize,
        "start_time": start_time,
        "end_time": end_time,
        "num_series": num_series,
    })
}

/// Query param for endpoints that select between baseline and experiment.
#[derive(serde::Deserialize)]
pub struct CaptureParam {
    #[serde(default)]
    pub capture: Option<String>,
}

impl CaptureParam {
    pub fn capture_id(&self) -> CaptureId {
        CaptureId::parse_opt(self.capture.as_deref())
    }
}

/// Standard JSON envelope for API endpoints. Matches Prometheus's
/// `{ status, data?, error?, errorType? }` shape.
#[derive(serde::Serialize)]
pub struct ApiResponse<T: serde::Serialize> {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "errorType")]
    error_type: Option<String>,
}

impl<T: serde::Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            status: "success".to_string(),
            data: Some(data),
            error: None,
            error_type: None,
        }
    }

    pub fn error(error: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            status: "error".to_string(),
            data: None,
            error: Some(error.into()),
            error_type: Some(error_type.into()),
        }
    }

    /// Convenience: build an error response already wrapped in `Json`.
    pub fn err(
        error: impl Into<String>,
        error_type: impl Into<String>,
    ) -> axum::response::Json<Self> {
        axum::response::Json(Self::error(error, error_type))
    }

    pub fn ok(data: T) -> axum::response::Json<Self> {
        axum::response::Json(Self::success(data))
    }
}

#[cfg(test)]
mod report_marker_tests {
    use super::*;
    use ::dashboard::TemplateRegistry;

    #[test]
    fn default_is_not_a_trimmed_report() {
        let state = AppState::new_empty(TemplateRegistry::empty());
        assert!(!state.is_trimmed_report());
    }

    #[test]
    fn setting_marker_flips_predicate() {
        let state = AppState::new_empty(TemplateRegistry::empty());
        *state.trimmed_report_marker.write() = Some("trimmed".to_string());
        assert!(state.is_trimmed_report());
    }
}
