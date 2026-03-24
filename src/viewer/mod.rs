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

#[cfg(feature = "developer-mode")]
use notify::Watcher;

#[cfg(feature = "developer-mode")]
use tower_http::services::{ServeDir, ServeFile};

#[cfg(feature = "developer-mode")]
use std::path::Path;

#[cfg(not(feature = "developer-mode"))]
static ASSETS: Dir<'_> = include_dir!("src/viewer/assets");

mod dashboard;
mod plot;

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
}

pub fn command() -> Command {
    Command::new("view")
        .about("View a Rezolus artifact or live agent")
        .arg(
            clap::Arg::new("INPUT")
                .help("Rezolus parquet file or agent URL (e.g. http://localhost:4241)")
                .action(clap::ArgAction::Set)
                .required(true)
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
        let input = args
            .get_one::<String>("INPUT")
            .expect("INPUT is required")
            .clone();

        let source = if input.starts_with("http://") || input.starts_with("https://") {
            Source::Live(
                input
                    .parse::<Url>()
                    .map_err(|e| format!("invalid URL: {e}"))?,
            )
        } else {
            Source::File(PathBuf::from(input))
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

            info!("Generating dashboards...");
            dashboard::generate(data)
        }
        Source::Live(url) => {
            info!("Connecting to live agent at {url}...");

            // Fetch agent version from the root endpoint
            let (source, version) = rt.block_on(async {
                let client = Client::builder()
                    .http1_only()
                    .build()
                    .expect("failed to create http client");
                match client.get(url.clone()).send().await {
                    Ok(response) => match response.text().await {
                        Ok(body) => {
                            // Parse "Rezolus 5.4.0 Agent\n..."
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
                        eprintln!("failed to connect to agent at {url}: {e}");
                        std::process::exit(1);
                    }
                }
            });

            info!("Connected to {source} {version} at {url}");

            // Generate dashboards from a TSDB with live metadata
            let mut tsdb = Tsdb::default();
            tsdb.set_sampling_interval_ms(1000);
            tsdb.set_source(source.clone());
            tsdb.set_version(version.clone());
            tsdb.set_filename(url.to_string());
            let mut state = dashboard::generate(tsdb);
            state.live = true;

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
    sections: HashMap<String, String>,
    tsdb: Arc<parking_lot::RwLock<Tsdb>>,
    /// Raw msgpack snapshot bytes for parquet export (live mode only).
    snapshots: Arc<parking_lot::Mutex<VecDeque<Vec<u8>>>>,
    live: bool,
}

impl AppState {
    pub fn new(tsdb: Tsdb) -> Self {
        Self {
            sections: Default::default(),
            tsdb: Arc::new(parking_lot::RwLock::new(tsdb)),
            snapshots: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
            live: false,
        }
    }
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
) -> (StatusCode, String) {
    (
        StatusCode::OK,
        state
            .sections
            .get(&path)
            .map(|v| v.to_string())
            .unwrap_or("{ }".to_string()),
    )
}

/// Returns whether the viewer is in live or file mode.
async fn mode(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<serde_json::Value> {
    axum::response::Json(serde_json::json!({
        "live": state.live,
    }))
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
    let metadata = serde_json::json!({
        "minTime": time_range.0,
        "maxTime": time_range.1
    });
    axum::response::Json(ApiResponse::success(metadata))
}

/// Reset the TSDB — clears all data and buffered snapshots.
async fn reset_tsdb(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    if !state.live {
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

        if let Some(info) = systeminfo::summary() {
            if let Ok(json) = serde_json::to_string(&info) {
                converter = converter.metadata("systeminfo".to_string(), json);
            }
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
