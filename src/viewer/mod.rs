use super::*;

#[cfg(not(feature = "developer-mode"))]
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::ArgMatches;
use http::header;
use http::StatusCode;
#[cfg(not(feature = "developer-mode"))]
use http::Uri;
#[cfg(not(feature = "developer-mode"))]
use include_dir::{include_dir, Dir};
use serde::Serialize;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;
use tower_livereload::LiveReloadLayer;

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "developer-mode")]
use notify::Watcher;

#[cfg(feature = "developer-mode")]
use tower_http::services::{ServeDir, ServeFile};

#[cfg(not(feature = "developer-mode"))]
static ASSETS: Dir<'_> = include_dir!("src/viewer/assets");

mod dashboard;
mod plot;
mod service_extension;
pub use service_extension::{Kpi, ServiceExtension};

// Re-export from metriken-query crate
pub use metriken_query::promql;
pub use metriken_query::tsdb;

use plot::*;
use promql::QueryEngine;
use tsdb::*;

/// The input source for the viewer.
enum Source {
    /// A parquet file on disk.
    File(PathBuf),
    /// A live Rezolus agent URL.
    Live(Url),
    /// No input — upload-only mode.
    Empty,
}

pub fn command() -> Command {
    Command::new("view")
        .about("View a Rezolus artifact or live agent")
        .arg(
            clap::Arg::new("INPUT")
                .help("Rezolus parquet file or agent URL (e.g. http://localhost:4241)")
                .action(clap::ArgAction::Set)
                .required(false)
                .index(1),
        )
        .arg(
            clap::Arg::new("VERBOSE")
                .long("verbose")
                .short('v')
                .help("Increase the verbosity")
                .action(clap::ArgAction::Count),
        )
        .arg(
            clap::Arg::new("LISTEN")
                .help("Viewer listen address")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(SocketAddr))
                .index(2),
        )
}

pub struct Config {
    source: Source,
    verbose: u8,
    listen: SocketAddr,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        let source = match args.get_one::<String>("INPUT") {
            Some(input) => {
                if input.starts_with("http://") || input.starts_with("https://") {
                    Source::Live(
                        input
                            .parse::<Url>()
                            .map_err(|e| format!("invalid URL: {e}"))?,
                    )
                } else {
                    Source::File(PathBuf::from(input))
                }
            }
            None => Source::Empty,
        };

        Ok(Config {
            source,
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            listen: *args
                .get_one::<SocketAddr>("LISTEN")
                .unwrap_or(&"127.0.0.1:0".to_socket_addrs().unwrap().next().unwrap()),
        })
    }
}

pub fn run(config: Config) {
    let config: Arc<Config> = config.into();

    // configure logging
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    ctrlc::set_handler(move || {
        std::process::exit(2);
    })
    .expect("failed to set ctrl-c handler");

    let state = match &config.source {
        Source::File(path) => {
            info!("Loading data from parquet file...");
            let data = Tsdb::load(path)
                .map_err(|e| {
                    eprintln!("failed to load data from parquet: {e}");
                    std::process::exit(1);
                })
                .unwrap();

            let filesize = std::fs::metadata(path).map(|m| m.len()).ok();

            let (systeminfo, selection) = extract_parquet_metadata(path);
            let service_ext = extract_service_extension_metadata(path);

            // Compute SHA-256 checksum of the parquet data (excluding footer metadata).
            // Parquet layout: [magic 4B] [row groups...] [footer] [footer_len 4B] [magic 4B]
            // We hash only [0, file_size - 8 - footer_len) so the checksum is stable
            // regardless of key-value metadata changes (e.g. selection annotations).
            info!("Computing file checksum...");
            let file_checksum = compute_file_checksum(path);

            if let Some(ref ext) = service_ext {
                info!(
                    "Found service extension for {:?} ({} KPIs)",
                    ext.service_name,
                    ext.kpis.len()
                );
            }

            info!("Generating dashboards...");
            let state = dashboard::generate(data, filesize, service_ext.as_ref());
            *state.parquet_path.write() = Some(path.clone());
            *state.systeminfo.write() = systeminfo;
            *state.selection.write() = selection;
            *state.file_checksum.write() = file_checksum;
            state
        }
        Source::Live(url) => {
            info!("Connecting to live agent at {url}...");

            // Fetch agent version and systeminfo from the agent
            let (source, version, agent_systeminfo) = rt.block_on(async {
                let client = Client::builder()
                    .http1_only()
                    .build()
                    .expect("failed to create http client");

                // Fetch version from root endpoint
                let (source, version) = match client.get(url.clone()).send().await {
                    Ok(response) => match response.text().await {
                        Ok(body) => {
                            let first_line = body.lines().next().unwrap_or("");
                            let parts: Vec<&str> = first_line.split_whitespace().collect();
                            match parts.as_slice() {
                                [name, ver, ..] => (name.to_string(), ver.to_string()),
                                _ => {
                                    warn!("unexpected agent banner: {first_line:?}");
                                    ("rezolus".to_string(), String::new())
                                }
                            }
                        }
                        Err(e) => {
                            warn!("failed to read agent banner: {e}");
                            ("rezolus".to_string(), String::new())
                        }
                    },
                    Err(e) => {
                        eprintln!("failed to connect to agent at {url}: {e}");
                        std::process::exit(1);
                    }
                };

                // Fetch systeminfo from agent
                let mut info_url = url.clone();
                info_url.set_path("/systeminfo");
                let sysinfo = match client.get(info_url).send().await {
                    Ok(response) if response.status().is_success() => response.text().await.ok(),
                    _ => None,
                };

                (source, version, sysinfo)
            });

            info!("Connected to {source} {version} at {url}");

            // Generate dashboards from a TSDB with live metadata
            let mut tsdb = Tsdb::default();
            tsdb.set_sampling_interval_ms(1000);
            tsdb.set_source(source.clone());
            tsdb.set_version(version.clone());
            tsdb.set_filename(url.to_string());
            let state = dashboard::generate(tsdb, None, None);
            state.live.store(true, Ordering::Relaxed);

            *state.systeminfo.write() = agent_systeminfo;

            // Spawn the ingest loop
            let ingest_tsdb = state.tsdb.clone();
            let ingest_snapshots = state.snapshots.clone();
            let mut ingest_url = url.clone();
            ingest_url.set_path("/metrics/binary");

            rt.spawn(ingest_loop(
                ingest_url,
                ingest_tsdb,
                ingest_snapshots,
                source,
                version,
            ));

            state
        }
        Source::Empty => {
            info!("No input file — starting in upload-only mode");
            AppState::new(Tsdb::default())
        }
    };

    // open the tcp listener
    let listener = std::net::TcpListener::bind(config.listen).expect("failed to listen");
    let addr = listener.local_addr().expect("socket missing local addr");

    // open in browser
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;

        if open::that(format!("http://{addr}")).is_err() {
            info!("Use your browser to view: http://{addr}");
        } else {
            info!("Launched browser to view: http://{addr}");
        }
    });

    // launch the HTTP listener
    rt.block_on(async move { serve(listener, state).await });

    std::thread::sleep(Duration::from_millis(200));
}

/// Background task that polls a live agent and ingests snapshots into the TSDB.
async fn ingest_loop(
    url: Url,
    tsdb: Arc<parking_lot::RwLock<Tsdb>>,
    snapshots: Arc<parking_lot::Mutex<VecDeque<Vec<u8>>>>,
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

    // Initialize the shared TSDB metadata
    {
        let mut tsdb = tsdb.write();
        tsdb.set_sampling_interval_ms(1000);
        tsdb.set_source(source);
        tsdb.set_version(version);
        tsdb.set_filename(url.to_string());
    }

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

        let latency = start.elapsed();
        debug!("sampling latency: {} us", latency.as_micros());

        let snapshot: metriken_exposition::Snapshot = match rmp_serde::from_slice(&body) {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to deserialize snapshot: {e}");
                continue;
            }
        };

        // Write directly to the shared TSDB — no cloning
        let mut tsdb = tsdb.write();
        tsdb.ingest(snapshot);
        sample_count += 1;

        // Buffer raw bytes for parquet export
        snapshots.lock().push_back(body.to_vec());

        if sample_count <= 5 || sample_count.is_multiple_of(60) {
            debug!(
                "ingested {} samples, counters: {}, gauges: {}, histograms: {}",
                sample_count,
                tsdb.counter_names().len(),
                tsdb.gauge_names().len(),
                tsdb.histogram_names().len(),
            );
        }
    }
}

async fn serve(listener: std::net::TcpListener, state: AppState) {
    let livereload = LiveReloadLayer::new();

    #[cfg(feature = "developer-mode")]
    {
        let reloader = livereload.reloader();

        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(event) = res {
                // Reload, unless it's just a read.
                if !matches!(event.kind, notify::EventKind::Access(_)) {
                    reloader.reload();
                }
            }
        })
        .expect("failed to initialize watcher");

        watcher
            .watch(
                Path::new("src/viewer/assets"),
                notify::RecursiveMode::Recursive,
            )
            .expect("failed to watch assets folder");
    }

    let app = app(livereload, state);

    listener.set_nonblocking(true).unwrap();
    let listener = TcpListener::from_std(listener).unwrap();

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

struct AppState {
    sections: parking_lot::RwLock<HashMap<String, String>>,
    tsdb: Arc<parking_lot::RwLock<Tsdb>>,
    /// Raw msgpack snapshot bytes for parquet export (live mode only).
    snapshots: Arc<parking_lot::Mutex<VecDeque<Vec<u8>>>>,
    live: AtomicBool,
    /// Original parquet file path (file mode only).
    parquet_path: parking_lot::RwLock<Option<std::path::PathBuf>>,
    /// Serialized SystemSummary JSON from parquet metadata or live system.
    systeminfo: parking_lot::RwLock<Option<String>>,
    /// Serialized selection JSON from parquet metadata.
    selection: parking_lot::RwLock<Option<String>>,
    /// SHA-256 hex digest of the source parquet file (file mode only).
    file_checksum: parking_lot::RwLock<Option<String>>,
}

impl AppState {
    pub fn new(tsdb: Tsdb) -> Self {
        Self {
            sections: Default::default(),
            tsdb: Arc::new(parking_lot::RwLock::new(tsdb)),
            snapshots: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
            live: AtomicBool::new(false),
            parquet_path: parking_lot::RwLock::new(None),
            systeminfo: parking_lot::RwLock::new(None),
            selection: parking_lot::RwLock::new(None),
            file_checksum: parking_lot::RwLock::new(None),
        }
    }
}

fn extract_parquet_metadata(path: &Path) -> (Option<String>, Option<String>) {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;
    std::fs::File::open(path)
        .ok()
        .and_then(|f| {
            let reader = SerializedFileReader::new(f).ok()?;
            let kv = reader.metadata().file_metadata().key_value_metadata()?;
            let sysinfo = kv
                .iter()
                .find(|kv| kv.key == "systeminfo")
                .and_then(|kv| kv.value.clone());
            let sel = kv
                .iter()
                .find(|kv| kv.key == "selection")
                .and_then(|kv| kv.value.clone());
            Some((sysinfo, sel))
        })
        .unwrap_or((None, None))
}

/// Search for service_queries inside the nested `metadata` map.
/// Scans all sources and returns the first ServiceExtension found.
fn extract_service_extension_metadata(path: &Path) -> Option<ServiceExtension> {
    use crate::parquet_metadata::{KEY_METADATA, NESTED_SERVICE_QUERIES};
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    let f = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(f).ok()?;
    let kv = reader.metadata().file_metadata().key_value_metadata()?;

    let metadata_json = kv
        .iter()
        .find(|kv| kv.key == KEY_METADATA)
        .and_then(|kv| kv.value.as_deref())?;

    let metadata_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(metadata_json).ok()?;

    for (_source, value) in &metadata_map {
        if let Some(sq) = value.get(NESTED_SERVICE_QUERIES) {
            if let Ok(ext) = serde_json::from_value::<ServiceExtension>(sq.clone()) {
                return Some(ext);
            }
        }
    }

    None
}

fn compute_file_checksum(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};
    (|| -> Option<String> {
        let mut f = std::fs::File::open(path).ok()?;
        let file_len = f.metadata().ok()?.len();
        if file_len < 12 {
            return None;
        }
        f.seek(SeekFrom::End(-8)).ok()?;
        let mut tail = [0u8; 4];
        f.read_exact(&mut tail).ok()?;
        let footer_len = u32::from_le_bytes(tail) as u64;
        let data_end = file_len.checked_sub(8 + footer_len)?;
        f.seek(SeekFrom::Start(0)).ok()?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut remaining = data_end;
        while remaining > 0 {
            let to_read = (remaining as usize).min(buf.len());
            match f.read(&mut buf[..to_read]) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                    remaining -= n as u64;
                }
                Err(e) => {
                    warn!("failed to read file for checksum: {e}");
                    return None;
                }
            }
        }
        Some(format!("{:x}", hasher.finalize()))
    })()
}

fn app(livereload: LiveReloadLayer, state: AppState) -> Router {
    let state = Arc::new(state);

    // API routes get Cache-Control: no-store to prevent browsers from
    // returning stale data during live mode polling.
    let api_routes = Router::new()
        .route("/query", get(instant_query))
        .route("/query_range", get(range_query))
        .route("/labels", get(label_names))
        .route("/label/{name}/values", get(label_values))
        .route("/metadata", get(metadata))
        .route("/mode", get(mode))
        .route("/reset", axum::routing::post(reset_tsdb))
        .route("/save", get(save_parquet))
        .route("/systeminfo", get(systeminfo_handler))
        .route("/selection", get(selection_handler))
        .route("/upload", axum::routing::post(upload_parquet))
        .route("/connect", axum::routing::post(connect_agent))
        .route(
            "/save_with_selection",
            axum::routing::post(save_with_selection),
        )
        .layer(axum::middleware::map_response(
            |mut response: axum::response::Response| async move {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                response
            },
        ));

    let router = Router::new()
        .route("/about", get(about))
        .route("/data/{path}", get(data))
        .nest("/api/v1", api_routes)
        .with_state(state.clone());

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

// Basic /about page handler
async fn about() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Viewer\nFor information, see: https://rezolus.com\n")
}

async fn data(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let sections = state.sections.read();
    match sections.get(&path) {
        Some(v) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            v.to_string(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Returns whether the viewer is in live or file mode.
async fn mode(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<serde_json::Value> {
    let loaded = !state.sections.read().is_empty();
    axum::response::Json(serde_json::json!({
        "live": state.live.load(Ordering::Relaxed),
        "loaded": loaded,
    }))
}

/// Returns the system hardware summary from parquet metadata or the live system.
async fn systeminfo_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    match &*state.systeminfo.read() {
        Some(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn selection_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

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

// PromQL API handlers that delegate to the swappable QueryEngine

#[derive(serde::Deserialize)]
struct QueryParams {
    query: String,
    time: Option<f64>,
}

#[derive(serde::Deserialize)]
struct RangeQueryParams {
    query: String,
    start: f64,
    end: f64,
    step: f64,
}

#[derive(serde::Serialize)]
struct ApiResponse<T: serde::Serialize> {
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
    fn success(data: T) -> Self {
        Self {
            status: "success".to_string(),
            data: Some(data),
            error: None,
            error_type: None,
        }
    }

    fn error(error: String, error_type: String) -> Self {
        Self {
            status: "error".to_string(),
            data: None,
            error: Some(error),
            error_type: Some(error_type),
        }
    }
}

fn error_type(e: &promql::QueryError) -> &'static str {
    match e {
        promql::QueryError::ParseError(_) => "bad_data",
        promql::QueryError::EvaluationError(_) => "execution",
        promql::QueryError::Unsupported(_) => "unsupported",
        promql::QueryError::MetricNotFound(_) => "not_found",
    }
}

async fn instant_query(
    axum::extract::Query(params): axum::extract::Query<QueryParams>,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<promql::QueryResult>> {
    let tsdb = state.tsdb.read();
    let engine = QueryEngine::new(&*tsdb);
    match engine.query(&params.query, params.time) {
        Ok(result) => axum::response::Json(ApiResponse::success(result)),
        Err(e) => axum::response::Json(ApiResponse::error(
            e.to_string(),
            error_type(&e).to_string(),
        )),
    }
}

async fn range_query(
    axum::extract::Query(params): axum::extract::Query<RangeQueryParams>,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<promql::QueryResult>> {
    let tsdb = state.tsdb.read();
    let engine = QueryEngine::new(&*tsdb);
    match engine.query_range(&params.query, params.start, params.end, params.step) {
        Ok(result) => axum::response::Json(ApiResponse::success(result)),
        Err(e) => axum::response::Json(ApiResponse::error(
            e.to_string(),
            error_type(&e).to_string(),
        )),
    }
}

async fn label_names(
    axum::extract::State(_state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<Vec<String>>> {
    let labels = vec![
        "__name__".to_string(),
        "direction".to_string(),
        "op".to_string(),
        "state".to_string(),
        "reason".to_string(),
        "id".to_string(),
        "name".to_string(),
        "sampler".to_string(),
    ];
    axum::response::Json(ApiResponse::success(labels))
}

async fn label_values(
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::State(_state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<Vec<String>>> {
    let values = match name.as_str() {
        "direction" => vec![
            "transmit".to_string(),
            "receive".to_string(),
            "to".to_string(),
            "from".to_string(),
        ],
        "op" => vec!["read".to_string(), "write".to_string()],
        "state" => vec!["user".to_string(), "system".to_string()],
        _ => vec![],
    };
    axum::response::Json(ApiResponse::success(values))
}

async fn metadata(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    let tsdb = state.tsdb.read();
    let engine = QueryEngine::new(&*tsdb);
    let time_range = engine.get_time_range();
    let mut metadata = serde_json::json!({
        "minTime": time_range.0,
        "maxTime": time_range.1
    });
    if let Some(checksum) = &*state.file_checksum.read() {
        metadata["fileChecksum"] = serde_json::json!(checksum);
    }
    axum::response::Json(ApiResponse::success(metadata))
}

/// Upload and load a parquet file into file-mode viewer state.
async fn upload_parquet(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return axum::response::Json(ApiResponse::error(
            "upload is only available in file mode".to_string(),
            "bad_request".to_string(),
        ));
    }

    if body.is_empty() {
        return axum::response::Json(ApiResponse::error(
            "missing parquet bytes".to_string(),
            "bad_request".to_string(),
        ));
    }

    let filename = headers
        .get("x-rezolus-filename")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
        .unwrap_or_else(|| "upload.parquet".to_string());

    let temp_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp_path = std::env::temp_dir().join(format!(
        "rezolus-viewer-{}-{}",
        std::process::id(),
        temp_suffix
    ));
    if let Err(e) = std::fs::write(&temp_path, &body) {
        return axum::response::Json(ApiResponse::error(
            format!("failed to store upload: {e}"),
            "io_error".to_string(),
        ));
    }

    let loaded = Tsdb::load(&temp_path);
    let mut data = match loaded {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return axum::response::Json(ApiResponse::error(
                format!("failed to load parquet: {e}"),
                "invalid_parquet".to_string(),
            ));
        }
    };

    let filesize = std::fs::metadata(&temp_path).map(|m| m.len()).ok();

    // Set the real filename before generating dashboards so the section JSON
    // embeds the original name instead of the temp path.
    data.set_filename(filename.clone());

    let service_ext = extract_service_extension_metadata(&temp_path);
    let new_state = dashboard::generate(data, filesize, service_ext.as_ref());
    let (systeminfo, selection) = extract_parquet_metadata(&temp_path);
    let file_checksum = compute_file_checksum(&temp_path);

    {
        let mut tsdb = state.tsdb.write();
        *tsdb = Arc::try_unwrap(new_state.tsdb)
            .ok()
            .expect("no other references to new tsdb")
            .into_inner();
    }
    {
        let mut sections = state.sections.write();
        *sections = new_state.sections.into_inner();
    }
    *state.parquet_path.write() = Some(temp_path);
    *state.systeminfo.write() = systeminfo;
    *state.selection.write() = selection;
    *state.file_checksum.write() = file_checksum;

    axum::response::Json(ApiResponse::success(serde_json::json!({
        "filename": filename,
    })))
}

/// Connect to a live Rezolus agent at runtime.
/// Fetches agent metadata, generates dashboards, spawns the ingest loop,
/// and flips the viewer into live mode.
async fn connect_agent(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return axum::response::Json(ApiResponse::error(
            "already connected to a live agent".to_string(),
            "bad_request".to_string(),
        ));
    }

    let url_str = match std::str::from_utf8(&body) {
        Ok(s) => s.trim().to_string(),
        Err(_) => {
            return axum::response::Json(ApiResponse::error(
                "invalid UTF-8 in URL".to_string(),
                "bad_request".to_string(),
            ));
        }
    };

    let url: Url = match url_str.parse() {
        Ok(u) => u,
        Err(e) => {
            return axum::response::Json(ApiResponse::error(
                format!("invalid URL: {e}"),
                "bad_request".to_string(),
            ));
        }
    };

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            return axum::response::Json(ApiResponse::error(
                format!("failed to create HTTP client: {e}"),
                "internal_error".to_string(),
            ));
        }
    };

    // Fetch agent version from root endpoint
    let (source, version) = match client.get(url.clone()).send().await {
        Ok(response) => match response.text().await {
            Ok(body) => {
                let first_line = body.lines().next().unwrap_or("");
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                match parts.as_slice() {
                    [name, ver, ..] => (name.to_string(), ver.to_string()),
                    _ => ("rezolus".to_string(), String::new()),
                }
            }
            Err(_) => ("rezolus".to_string(), String::new()),
        },
        Err(e) => {
            return axum::response::Json(ApiResponse::error(
                format!("failed to connect to agent at {url}: {e}"),
                "connection_error".to_string(),
            ));
        }
    };

    // Fetch systeminfo from agent
    let mut info_url = url.clone();
    info_url.set_path("/systeminfo");
    let agent_systeminfo = match client.get(info_url).send().await {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };

    // Generate dashboards with live metadata
    let mut tsdb = Tsdb::default();
    tsdb.set_sampling_interval_ms(1000);
    tsdb.set_source(source.clone());
    tsdb.set_version(version.clone());
    tsdb.set_filename(url.to_string());
    let new_state = dashboard::generate(tsdb, None, None);

    // Update shared state
    {
        let mut tsdb = state.tsdb.write();
        *tsdb = Arc::try_unwrap(new_state.tsdb)
            .ok()
            .expect("no other references to new tsdb")
            .into_inner();
    }
    {
        let mut sections = state.sections.write();
        *sections = new_state.sections.into_inner();
    }
    *state.systeminfo.write() = agent_systeminfo;
    state.live.store(true, Ordering::Relaxed);

    // Spawn the ingest loop
    let ingest_tsdb = state.tsdb.clone();
    let ingest_snapshots = state.snapshots.clone();
    let mut ingest_url = url.clone();
    ingest_url.set_path("/metrics/binary");

    tokio::spawn(ingest_loop(
        ingest_url,
        ingest_tsdb,
        ingest_snapshots,
        source.clone(),
        version.clone(),
    ));

    info!("Connected to {source} {version} at {url}");

    axum::response::Json(ApiResponse::success(serde_json::json!({
        "source": source,
        "version": version,
        "url": url.to_string(),
    })))
}

/// Reset the TSDB — clears all data and buffered snapshots.
async fn reset_tsdb(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    if !state.live.load(Ordering::Relaxed) {
        return axum::response::Json(ApiResponse::error(
            "reset is only available in live mode".to_string(),
            "bad_request".to_string(),
        ));
    }

    // Preserve metadata across reset
    let (source, version, filename) = {
        let tsdb = state.tsdb.read();
        (
            tsdb.source().to_string(),
            tsdb.version().to_string(),
            tsdb.filename().to_string(),
        )
    };

    {
        let mut tsdb = state.tsdb.write();
        *tsdb = Tsdb::default();
        tsdb.set_sampling_interval_ms(1000);
        tsdb.set_source(source);
        tsdb.set_version(version);
        tsdb.set_filename(filename);
    }

    state.snapshots.lock().clear();
    info!("TSDB reset by user");

    axum::response::Json(ApiResponse::success(serde_json::json!({ "ok": true })))
}

/// Save buffered snapshots as a parquet file download.
async fn save_parquet(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::body::Body;
    use std::io::Cursor;

    let snapshot_data: Vec<Vec<u8>> = state.snapshots.lock().iter().cloned().collect();

    if snapshot_data.is_empty() {
        return axum::response::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    }

    // Grab the stored systeminfo (from agent or local) for parquet metadata
    let sysinfo_json = state.systeminfo.read().clone();

    // Run the synchronous parquet conversion off the async runtime
    let result = tokio::task::spawn_blocking(move || {
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

        converter
            .convert_file_handle(reader, Cursor::new(&mut output))
            .map(|rows| {
                info!("saved parquet with {rows} rows");
                output
            })
    })
    .await;

    match result {
        Ok(Ok(output)) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"rezolus-capture.parquet\"",
            )
            .body(Body::from(output))
            .unwrap(),
        Ok(Err(e)) => {
            error!("failed to convert to parquet: {e}");
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("parquet conversion failed: {e}")))
                .unwrap()
        }
        Err(e) => {
            error!("parquet conversion task panicked: {e}");
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("internal error"))
                .unwrap()
        }
    }
}

async fn save_with_selection(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    body: String,
) -> axum::response::Response {
    use axum::body::Body;
    use std::io::Cursor;

    let parquet_path = state.parquet_path.read().clone();
    let selection_json = body;

    // File mode: copy original parquet with selection metadata added
    if let Some(path) = parquet_path {
        let result = tokio::task::spawn_blocking(move || {
            use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
            use parquet::arrow::ArrowWriter;
            use parquet::file::properties::WriterProperties;
            use parquet::file::reader::FileReader;
            use parquet::file::serialized_reader::SerializedFileReader;
            use parquet::format::KeyValue;

            let file = std::fs::File::open(&path)?;

            // Read existing metadata to preserve and augment
            let meta_reader = SerializedFileReader::new(std::fs::File::open(&path)?)?;
            let mut kv_meta: Vec<KeyValue> = meta_reader
                .metadata()
                .file_metadata()
                .key_value_metadata()
                .cloned()
                .unwrap_or_default();
            kv_meta.retain(|kv| kv.key != "selection");
            kv_meta.push(KeyValue {
                key: "selection".to_string(),
                value: Some(selection_json),
            });

            let props = WriterProperties::builder()
                .set_key_value_metadata(Some(kv_meta))
                .build();

            let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
            let schema = builder.schema().clone();
            let reader = builder.build()?;

            let mut output = Vec::new();
            {
                let mut writer =
                    ArrowWriter::try_new(Cursor::new(&mut output), schema, Some(props))?;
                for batch in reader {
                    let batch = batch?;
                    writer.write(&batch)?;
                }
                writer.close()?;
            }

            info!("saved annotated parquet ({} bytes)", output.len());
            Ok::<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>(output)
        })
        .await;

        return match result {
            Ok(Ok(output)) => axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"rezolus-capture-annotated.parquet\"",
                )
                .body(Body::from(output))
                .unwrap(),
            Ok(Err(e)) => {
                error!("failed to annotate parquet: {e}");
                axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(format!("parquet annotation failed: {e}")))
                    .unwrap()
            }
            Err(e) => {
                error!("parquet annotation task panicked: {e}");
                axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("internal error"))
                    .unwrap()
            }
        };
    }

    // Live mode: convert snapshots to parquet with selection metadata
    let snapshot_data: Vec<Vec<u8>> = state.snapshots.lock().iter().cloned().collect();

    if snapshot_data.is_empty() {
        return axum::response::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    }

    let sysinfo_json = state.systeminfo.read().clone();

    let result = tokio::task::spawn_blocking(move || {
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

        converter = converter.metadata("selection".to_string(), selection_json);

        converter
            .convert_file_handle(reader, Cursor::new(&mut output))
            .map(|rows| {
                info!("saved annotated parquet with {rows} rows");
                output
            })
    })
    .await;

    match result {
        Ok(Ok(output)) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"rezolus-capture-annotated.parquet\"",
            )
            .body(Body::from(output))
            .unwrap(),
        Ok(Err(e)) => {
            error!("failed to convert to parquet: {e}");
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("parquet conversion failed: {e}")))
                .unwrap()
        }
        Err(e) => {
            error!("parquet conversion task panicked: {e}");
            axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("internal error"))
                .unwrap()
        }
    }
}

#[cfg(not(feature = "developer-mode"))]
async fn index() -> impl IntoResponse {
    if let Some(asset) = ASSETS.get_file("index.html") {
        let body = asset.contents_utf8().unwrap();
        let content_type = "text/html";

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            body.to_string(),
        )
    } else {
        error!("index.html missing from build");
        (
            StatusCode::from_u16(404).unwrap(),
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found".to_string(),
        )
    }
}

#[cfg(not(feature = "developer-mode"))]
async fn lib(uri: Uri) -> impl IntoResponse {
    let path = uri.path();

    if let Some(asset) = ASSETS.get_file(format!("lib{path}")) {
        let body = asset.contents_utf8().unwrap();
        let content_type = if path.ends_with(".js") {
            "text/javascript"
        } else if path.ends_with(".css") {
            "text/css"
        } else if path.ends_with(".html") {
            "text/html"
        } else if path.ends_with(".json") {
            "application/json"
        } else {
            "text/plain"
        };

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            body.to_string(),
        )
    } else {
        error!("path: {path} does not map to a static resource");
        (
            StatusCode::from_u16(404).unwrap(),
            [(header::CONTENT_TYPE, "text/plain")],
            "404 Not Found".to_string(),
        )
    }
}

/// Dump all dashboard definitions as JSON files to the given directory.
/// Used by `cargo xtask generate-dashboards` to keep site viewer in sync.
pub fn dump_dashboards(output_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(output_dir)?;

    let state = dashboard::generate(Tsdb::default(), None, None);

    // Extract the shared sections list from the first entry and write it once.
    let mut sections_written = false;
    for (key, json) in state.sections.read().iter() {
        let mut value: serde_json::Value = serde_json::from_str(json)?;

        if !sections_written {
            if let Some(sections) = value.get("sections") {
                let path = output_dir.join("sections.json");
                let pretty = serde_json::to_string_pretty(sections)?;
                std::fs::write(&path, pretty)?;
                eprintln!("wrote {}", path.display());
                sections_written = true;
            }
        }

        // Remove sections from per-dashboard files to avoid duplication.
        if let Some(obj) = value.as_object_mut() {
            obj.remove("sections");
        }

        let path = output_dir.join(key);
        let pretty = serde_json::to_string_pretty(&value)?;
        std::fs::write(&path, pretty)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
