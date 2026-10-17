//! Viewer server state and shared types.
//!
//! `AppState` is the single Arc-shared handle threaded into every axum
//! handler. The ancillary types (`LazySectionStore`, `ProxyState`,
//! `ApiResponse`, `CaptureParam`) live here too because they're the
//! HTTP-level scaffolding the handlers all touch.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use reqwest::Client;
use tracing::error;

use metriken_query::MetricsSource;

use metriken_storage::BufferPool;

use super::capture_registry::{CaptureId, CaptureRegistry};
use super::proxy_allow;
use ::dashboard::{self, TemplateRegistry};

/// The least the server-side viewer's buffer pool budget defaults to, and the
/// default where available memory is not known: 500 MB.
pub const MIN_DEFAULT_CACHE_SIZE_BYTES: usize = 500 * 1024 * 1024;

/// The most the server-side viewer's buffer pool budget defaults to: 8 GB.
pub const MAX_DEFAULT_CACHE_SIZE_BYTES: usize = 8 * 1024 * 1024 * 1024;

/// Default buffer pool budget for the server-side viewer: an eighth of the
/// memory available to the process (physical memory, or a Linux cgroup's
/// limit when lower), clamped to [`MIN_DEFAULT_CACHE_SIZE_BYTES`] to
/// [`MAX_DEFAULT_CACHE_SIZE_BYTES`]. The row-group cache holds at most the
/// budget. Each table's segment cache holds at most the budget or that
/// table's segment bytes, whichever is smaller, so together they hold at
/// most the archive's segment bytes.
///
/// All readers share this budget via an LRU-evicted cache of decoded row
/// groups. The budget is a ceiling: the pool holds only the row groups
/// queries decode. First-query latency is unchanged; a query over row groups
/// already decoded (the common dashboard workload) is served from the cache.
/// A budget smaller than a query's row groups gives no reuse at all: every
/// repeat decodes them again.
///
/// Tune with `REZOLUS_CACHE_MB` (env) or the `--cache-size-mb` flag.
pub fn default_cache_size_bytes() -> usize {
    let available = match (physical_memory_bytes(), cgroup_memory_limit()) {
        (Some(p), Some(c)) => Some(p.min(c)),
        (p, c) => p.or(c),
    };
    cache_size_for(available)
}

/// The default budget for a host with `physical` bytes of memory, if known.
fn cache_size_for(physical: Option<usize>) -> usize {
    physical
        .map(|bytes| (bytes / 8).clamp(MIN_DEFAULT_CACHE_SIZE_BYTES, MAX_DEFAULT_CACHE_SIZE_BYTES))
        .unwrap_or(MIN_DEFAULT_CACHE_SIZE_BYTES)
}

/// Physical memory in bytes, where the platform reports it.
#[cfg(unix)]
fn physical_memory_bytes() -> Option<usize> {
    // SAFETY: `sysconf` reads a configuration value and has no
    // preconditions; a negative result is an error and is refused below.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let pages = usize::try_from(pages).ok()?;
    let page_size = usize::try_from(page_size).ok()?;
    pages.checked_mul(page_size).filter(|bytes| *bytes > 0)
}

#[cfg(not(unix))]
fn physical_memory_bytes() -> Option<usize> {
    None
}

/// The memory limit of the Linux cgroup the process runs in, if one is set
/// anywhere from its cgroup up to the root. A container's limit is lower than
/// the host's physical memory, which `sysconf` reports.
#[cfg(target_os = "linux")]
fn cgroup_memory_limit() -> Option<usize> {
    cgroup_limit_from(|path| std::fs::read_to_string(path).ok())
}

/// The cgroup memory limit, reading files through `read`: the tightest of
/// cgroup v2's `memory.max` and, where the memory controller is on the v1
/// hierarchy (v1 or hybrid hosts), v1's `memory.limit_in_bytes`, each from
/// this process's cgroup up to its hierarchy's root.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit_from(read: impl Fn(&std::path::Path) -> Option<String>) -> Option<usize> {
    use std::path::{Path, PathBuf};
    // The tightest limit in `file` from `root/rel` up to `root`. A level with
    // no limit ("max" on v2) does not parse and is skipped; v1's "no limit"
    // is a value near i64::MAX, which the minimum with physical memory
    // discards.
    let walk = |root: &Path, rel: &str, file: &str| {
        let mut dir: PathBuf = root.join(rel.trim_start_matches('/'));
        let mut limit: Option<usize> = None;
        loop {
            if let Some(v) = read(&dir.join(file)).and_then(|s| s.trim().parse::<usize>().ok()) {
                limit = Some(limit.map_or(v, |l| l.min(v)));
            }
            if dir == root || !dir.pop() {
                break;
            }
        }
        limit
    };
    let membership = read(Path::new("/proc/self/cgroup"))?;
    let v2 = membership
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .and_then(|rel| walk(Path::new("/sys/fs/cgroup"), rel, "memory.max"));
    let v1 = membership
        .lines()
        .find_map(|l| {
            let mut fields = l.splitn(3, ':');
            let (_, controllers, rel) = (fields.next()?, fields.next()?, fields.next()?);
            controllers.split(',').any(|c| c == "memory").then_some(rel)
        })
        .and_then(|rel| {
            walk(
                Path::new("/sys/fs/cgroup/memory"),
                rel,
                "memory.limit_in_bytes",
            )
        });
    match (v2, v1) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(not(target_os = "linux"))]
fn cgroup_memory_limit() -> Option<usize> {
    None
}

/// Caches the navigation list (via the owned `DashboardContext`) and
/// memoizes per-section JSON bodies. `/api/v1/sections` reads the nav
/// without materializing any body; `/data/<section>.json` generates a
/// body on first request and serves the cached value thereafter.
pub struct LazySectionStore {
    context: dashboard::dashboard::DashboardContext,
    cached_bodies: HashMap<String, serde_json::Value>,
    /// Distinct for every store made, so a section generated from one
    /// store is not cached in the store that replaced it.
    id: u64,
}

impl LazySectionStore {
    pub fn new(context: dashboard::dashboard::DashboardContext) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self {
            context,
            cached_bodies: HashMap::new(),
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// This store's id: see [`insert`](Self::insert).
    pub fn id(&self) -> u64 {
        self.id
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
        let key = Self::key(route);
        if !self.cached_bodies.contains_key(&key) {
            let value = self.generate(route, data)?;
            self.cached_bodies.insert(key.clone(), value);
        }
        self.cached_bodies.get(&key)
    }

    fn key(route: &str) -> String {
        format!("{}.json", &route[1..])
    }

    /// The section at `route` if it has been generated.
    pub fn cached(&self, route: &str) -> Option<&serde_json::Value> {
        self.cached_bodies.get(&Self::key(route))
    }

    /// Generate the section at `route` from `data`, without caching it.
    pub fn generate(&self, route: &str, data: &dyn MetricsSource) -> Option<serde_json::Value> {
        let mut view = dashboard::dashboard::generate_section(data, route, &self.context)?;
        view.set_filename(data.filename_or_default());
        if let Some(size) = self.context.filesize {
            view.set_filesize(size);
        }
        serde_json::to_value(&view).ok()
    }

    /// Cache `value` as the section at `route`, if it was generated from
    /// the store with id `from`, which is this one.
    pub fn insert(&mut self, from: u64, route: &str, value: serde_json::Value) {
        if from == self.id {
            self.cached_bodies.insert(Self::key(route), value);
        }
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
    /// Whose KPI template renders for a source that carries one in its
    /// metadata and that `templates` also knows (`--registry-templates`).
    pub template_preference: RwLock<::dashboard::source_templates::TemplatePreference>,
    /// The live agent being recorded (live mode only). Its temporary archive
    /// is the capture's data and what a save copies.
    pub live_session: Mutex<Option<super::live::LiveSession>>,
    pub live: AtomicBool,
    /// The opened archive file being followed while its writer appends
    /// (file mode only). Replacing the baseline ends it.
    pub follow: Mutex<Option<super::follow::Follow>>,
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
    /// Shared LRU row-group cache for all `ParquetReader`s in this process.
    ///
    /// Readers opened with `Arc::clone(&state.pool)` share the same budget.
    /// The default is `default_cache_size_bytes()`; set `REZOLUS_CACHE_MB` or
    /// pass `--cache-size-mb` to override.
    pub pool: Arc<BufferPool>,
    /// One permit per request that may read a recording at once: a query, a
    /// dashboard section, saving a report, or loading an upload, a URL or an
    /// experiment, or detaching the experiment. See
    /// [`default_query_concurrency`] and `--query-concurrency`.
    pub queries: Arc<tokio::sync::Semaphore>,
}

/// The default number of queries the viewer runs at once: half the CPUs,
/// at least 2 and at most 8. A batched read of archive segments uses up to
/// `min(CPUs, 8)` threads of its own, so N slots can run up to 8 * N read
/// threads.
pub fn default_query_concurrency() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get() / 2)
        .clamp(2, 8)
}

impl AppState {
    #[cfg(test)]
    pub fn new(data: Arc<dyn MetricsSource>, templates: TemplateRegistry) -> Self {
        Self::with_pool(data, templates, BufferPool::new(default_cache_size_bytes()))
    }

    pub fn with_pool(
        data: Arc<dyn MetricsSource>,
        templates: TemplateRegistry,
        pool: Arc<BufferPool>,
    ) -> Self {
        Self {
            sections: Default::default(),
            captures: Arc::new(CaptureRegistry::new(data, None, None, None)),
            templates,
            template_preference: RwLock::new(Default::default()),
            live_session: Mutex::new(None),
            live: AtomicBool::new(false),
            follow: Mutex::new(None),
            parquet_path: RwLock::new(None),
            experiment_parquet_path: RwLock::new(None),
            cli_experiment_path: RwLock::new(None),
            category_name: RwLock::new(None),
            selection: RwLock::new(None),
            file_checksum: RwLock::new(None),
            proxy: ProxyState::default(),
            combined_ab_marker: RwLock::new(None),
            trimmed_report_marker: RwLock::new(None),
            pool,
            queries: Arc::new(tokio::sync::Semaphore::new(default_query_concurrency())),
        }
    }

    /// Let `n` queries read the recording at once, at least 1.
    pub fn set_query_concurrency(&mut self, n: usize) {
        self.queries = Arc::new(tokio::sync::Semaphore::new(n.max(1)));
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

    /// Make `session` the live capture: its reader the baseline, its
    /// archive what saves copy. The baseline reader, `parquet_path` and the
    /// session slot change under the session lock, and
    /// a save reads the archive path and its hold under the same lock, so
    /// two resets racing each other leave the view, the saves and the
    /// running recording on the same session. The session replaced is
    /// dropped, which stops its recording.
    pub fn install_live(&self, session: super::live::LiveSession) {
        let mut slot = self.live_session.lock();
        self.replace_baseline(session.reader());
        *self.parquet_path.write() = Some(session.path().to_path_buf());
        *slot = Some(session);
    }

    /// Replace the baseline data store (used by upload/connect handlers).
    /// The display filename is carried on the data source itself. A followed
    /// archive stops being followed: it is no longer what the page shows.
    pub fn replace_baseline(&self, data: Arc<dyn MetricsSource>) {
        self.follow.lock().take();
        self.captures.set_baseline_data(data);
    }

    /// Stop reopening capture `id` of a followed archive: its slot was
    /// detached or is being replaced.
    pub fn unfollow_capture(&self, id: &str) {
        if let Some(follow) = self.follow.lock().as_ref() {
            follow.drop_capture(id);
        }
    }

    /// Whether the opened archive file is being followed. A follow ends when
    /// the file is finalized or removed, or the baseline is replaced.
    pub fn following(&self) -> bool {
        self.follow.lock().as_ref().is_some_and(|f| f.active())
    }

    pub fn combined_ab(&self) -> bool {
        self.combined_ab_marker.read().is_some()
    }

    /// Single lookup that hides the CLI-vs-HTTP-attach split; CLI wins.
    pub fn resolve_experiment_parquet_path(&self) -> Option<std::path::PathBuf> {
        self.cli_experiment_path
            .read()
            .clone()
            .or_else(|| self.experiment_parquet_path.read().clone())
    }

    pub fn is_trimmed_report(&self) -> bool {
        self.trimmed_report_marker.read().is_some()
    }

    /// Build the navigation + global params payload for `/api/v1/sections`.
    pub fn sections_metadata(&self) -> serde_json::Value {
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
        let filename = data.filename_or_default();
        // time_range is now in seconds; convert to milliseconds for the UI.
        let (start_time, end_time) = data
            .time_range()
            .map(|(min, max)| ((min * 1000.0) as u64, (max * 1000.0) as u64))
            .unwrap_or((0, 0));
        build_sections_metadata_payload(
            sections_array,
            &source,
            &version,
            &filename,
            interval,
            filesize,
            start_time,
            end_time,
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
    })
}

/// Query param for endpoints that select between baseline and experiment.
#[derive(serde::Deserialize)]
pub struct CaptureParam {
    #[serde(default)]
    pub capture: Option<String>,
}

impl CaptureParam {
    /// The raw wire id, defaulting to the anchor when absent.
    ///
    /// A string, never the `CaptureId` enum: an archive attaches captures
    /// under named ids (`envoy`), so a request for `capture=envoy` must
    /// resolve the `envoy` recording, not collapse an unknown id to the
    /// baseline. The registry's `*_by_id` accessors take this string.
    pub fn capture_str(&self) -> &str {
        self.capture
            .as_deref()
            .unwrap_or(super::capture_registry::BASELINE_ID)
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

#[cfg(test)]
mod cache_size_tests {
    use super::*;

    const GB: usize = 1024 * 1024 * 1024;

    #[test]
    fn the_default_budget_is_an_eighth_of_memory_within_its_bounds() {
        assert_eq!(cache_size_for(Some(16 * GB)), 2 * GB);
        assert_eq!(cache_size_for(Some(2 * GB)), MIN_DEFAULT_CACHE_SIZE_BYTES);
        assert_eq!(
            cache_size_for(Some(1024 * GB)),
            MAX_DEFAULT_CACHE_SIZE_BYTES
        );
        assert_eq!(cache_size_for(None), MIN_DEFAULT_CACHE_SIZE_BYTES);
    }

    fn files(entries: &[(&str, &str)]) -> impl Fn(&std::path::Path) -> Option<String> {
        let map: std::collections::HashMap<std::path::PathBuf, String> = entries
            .iter()
            .map(|(p, c)| (std::path::PathBuf::from(p), c.to_string()))
            .collect();
        move |path| map.get(path).cloned()
    }

    #[test]
    fn a_cgroup_v2_limit_is_the_tightest_up_to_the_root() {
        let read = files(&[
            ("/proc/self/cgroup", "0::/system.slice/view.service\n"),
            (
                "/sys/fs/cgroup/system.slice/view.service/memory.max",
                "max\n",
            ),
            ("/sys/fs/cgroup/system.slice/memory.max", "4294967296\n"),
            ("/sys/fs/cgroup/memory.max", "8589934592\n"),
        ]);
        assert_eq!(cgroup_limit_from(read), Some(4 * GB));
        let unlimited = files(&[
            ("/proc/self/cgroup", "0::/\n"),
            ("/sys/fs/cgroup/memory.max", "max\n"),
        ]);
        assert_eq!(cgroup_limit_from(unlimited), None);
    }

    #[test]
    fn a_cgroup_v1_limit_is_read() {
        let read = files(&[
            ("/proc/self/cgroup", "12:memory:/docker/abc\n"),
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "2147483648\n",
            ),
        ]);
        assert_eq!(cgroup_limit_from(read), Some(2 * GB));
        assert_eq!(cgroup_limit_from(files(&[])), None);
        // On a host, the process's own v1 cgroup sets the limit; the root
        // reads as unlimited.
        let nested = files(&[
            ("/proc/self/cgroup", "4:memory:/system.slice/view.service\n"),
            (
                "/sys/fs/cgroup/memory/system.slice/view.service/memory.limit_in_bytes",
                "1073741824\n",
            ),
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712\n",
            ),
        ]);
        assert_eq!(cgroup_limit_from(nested), Some(GB));
    }

    /// A hybrid host lists a `0::` line beside the v1 controllers, and its
    /// unified hierarchy has no memory controller: the v1 limit is read.
    #[test]
    fn a_hybrid_host_reads_the_v1_limit() {
        let read = files(&[
            (
                "/proc/self/cgroup",
                "12:memory:/docker/abc\n0::/system.slice/containerd.service\n",
            ),
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "2147483648\n",
            ),
        ]);
        assert_eq!(cgroup_limit_from(read), Some(2 * GB));
    }

    #[test]
    fn physical_memory_is_known_here() {
        // CI builds on Linux and macOS, both unix; elsewhere the reading is
        // `None` and the default is the floor.
        if cfg!(unix) {
            assert!(physical_memory_bytes().is_some_and(|b| b >= 256 * 1024 * 1024));
        }
    }
}
