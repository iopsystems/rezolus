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

use metriken_query::MetricsSource;

use super::capture_registry::{CaptureId, CaptureRegistry};
use super::proxy_allow;
use ::dashboard::{self, TemplateRegistry};

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
        data: &dyn MetricsSource,
    ) -> Option<&serde_json::Value> {
        let key = format!("{}.json", &route[1..]);
        if !self.cached_bodies.contains_key(&key) {
            let mut view =
                dashboard::dashboard::generate_section(data, route, &self.context)?;
            view.set_filename(data.filename().unwrap_or_default());
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

pub struct AppState {
    pub sections: RwLock<LazySectionStore>,
    /// Per-capture data store + metadata. Single-capture callers always target
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
    pub experiment_parquet_path: RwLock<Option<PathBuf>>,
    /// User-supplied experiment parquet path from the CLI.
    pub cli_experiment_path: RwLock<Option<PathBuf>>,
    /// Active category template name (when `--category` was supplied).
    pub category_name: RwLock<Option<String>>,
    /// Serialized selection JSON from parquet metadata.
    pub selection: RwLock<Option<String>>,
    /// SHA-256 hex digest of the source parquet file (file mode only).
    pub file_checksum: RwLock<Option<String>>,
    pub proxy: ProxyState,
    pub combined_ab_marker: RwLock<Option<crate::parquet_metadata::AbContainers>>,
    pub trimmed_report_marker: RwLock<Option<String>>,
}

impl AppState {
    pub fn new(
        data: Arc<dyn MetricsSource>,
        templates: TemplateRegistry,
    ) -> Self {
        Self {
            sections: Default::default(),
            captures: Arc::new(CaptureRegistry::new(data, None, None, None)),
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
        }
    }

    /// Enable the URL proxy with the given hostname allowlist.
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

    /// Shorthand for the baseline data store (clones the Arc).
    pub fn baseline_data(&self) -> Arc<dyn MetricsSource> {
        self.captures
            .get(CaptureId::Baseline)
            .expect("baseline capture is always present")
    }

    /// Replace the baseline data store (used by upload/connect handlers).
    /// The display filename is carried on the data source itself.
    pub fn replace_baseline(&self, data: Arc<dyn MetricsSource>) {
        self.captures.set_baseline_data(data);
    }

    pub fn combined_ab(&self) -> bool {
        self.combined_ab_marker.read().is_some()
    }

    pub fn is_trimmed_report(&self) -> bool {
        self.trimmed_report_marker.read().is_some()
    }

    /// Build the navigation + global params payload for `/api/v1/sections`.
    pub fn sections_metadata(&self, _capture: CaptureId) -> serde_json::Value {
        let store = self.sections.read();
        let sections_array: Vec<serde_json::Value> = store
            .sections()
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or_default())
            .collect();
        let filesize = store.context().filesize.unwrap_or(0);
        drop(store);

        let data = self.baseline_data();
        let interval = data.interval();
        let source = data.source();
        let version = data.version();
        let filename = data.filename().unwrap_or_default();
        // time_range is now in seconds; convert to milliseconds for the UI.
        let (start_time, end_time) = data
            .time_range()
            .map(|(min, max)| (
                (min * 1000.0) as u64,
                (max * 1000.0) as u64,
            ))
            .unwrap_or((0, 0));
        let num_series = {
            let mut count = 0usize;
            for name in data.counter_names() {
                count += data.counter_labels(&name).len();
            }
            for name in data.gauge_names() {
                count += data.gauge_labels(&name).len();
            }
            for name in data.histogram_names() {
                count += data.histogram_labels(&name).len();
            }
            count
        };

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

/// Standard JSON envelope for API endpoints.
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

pub fn promql_error_type(e: &metriken_query::QueryError) -> &'static str {
    use metriken_query::QueryError::*;
    match e {
        ParseError(_) => "bad_data",
        EvaluationError(_) => "execution",
        Unsupported(_) => "unsupported",
        MetricNotFound(_) => "not_found",
    }
}

#[cfg(test)]
mod report_marker_tests {
    use super::*;
    use ::dashboard::TemplateRegistry;
    use metriken_query::MemoryStore;

    #[test]
    fn default_is_not_a_trimmed_report() {
        let store = Arc::new(MemoryStore::builder().build()) as Arc<dyn MetricsSource>;
        let state = AppState::new(store, TemplateRegistry::empty());
        assert!(!state.is_trimmed_report());
    }

    #[test]
    fn setting_marker_flips_predicate() {
        let store = Arc::new(MemoryStore::builder().build()) as Arc<dyn MetricsSource>;
        let state = AppState::new(store, TemplateRegistry::empty());
        *state.trimmed_report_marker.write() = Some("trimmed".to_string());
        assert!(state.is_trimmed_report());
    }
}
