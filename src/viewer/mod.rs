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

#[cfg(test)]
pub use dashboard::Kpi;
pub use dashboard::{ServiceExtension, TemplateRegistry};

// Re-export from metriken-query crate
pub use metriken_query::promql;
pub use metriken_query::tsdb;

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
        .arg(
            clap::Arg::new("templates")
                .long("templates")
                .value_name("DIR")
                .help("Directory containing service extension template JSON files")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set),
        )
}

pub struct Config {
    source: Source,
    verbose: u8,
    listen: SocketAddr,
    templates_dir: Option<PathBuf>,
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
            templates_dir: args.get_one::<PathBuf>("templates").cloned(),
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

    let registry = TemplateRegistry::resolve_and_load(config.templates_dir.as_deref());

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

            let (systeminfo, selection, file_meta) = extract_parquet_metadata(path);
            let mut service_exts = extract_service_extension_metadata(path, &registry);

            // Validate KPI availability against the loaded data so that
            // template-derived dashboards hide KPIs with no data.
            let data_arc = std::sync::Arc::new(data);
            validate_service_extensions(&data_arc, &mut service_exts);
            let data = std::sync::Arc::try_unwrap(data_arc)
                .ok()
                .expect("Arc still shared");

            // Compute SHA-256 checksum of the parquet data (excluding footer metadata).
            // Parquet layout: [magic 4B] [row groups...] [footer] [footer_len 4B] [magic 4B]
            // We hash only [0, file_size - 8 - footer_len) so the checksum is stable
            // regardless of key-value metadata changes (e.g. selection annotations).
            info!("Computing file checksum...");
            let file_checksum = compute_file_checksum(path);

            for (source, ext) in &service_exts {
                let available = ext.kpis.iter().filter(|k| k.available).count();
                info!(
                    "Found service extension for {:?} from source {:?} ({}/{} KPIs available)",
                    ext.service_name,
                    source,
                    available,
                    ext.kpis.len()
                );
            }

            info!("Generating dashboards...");
            let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
            let state = AppState::new(data, registry.clone());
            let rendered =
                dashboard::dashboard::generate(&state.tsdb.read(), filesize, &service_refs, None);
            state.sections.write().extend(rendered);
            *state.parquet_path.write() = Some(path.clone());
            let multinode_sysinfo = build_multinode_systeminfo(path);
            *state.systeminfo.write() = multinode_sysinfo.or(systeminfo);
            *state.selection.write() = selection;
            *state.file_checksum.write() = file_checksum;
            *state.file_metadata.write() = file_meta;
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
            let state = AppState::new(tsdb, registry.clone());
            let rendered = dashboard::dashboard::generate(&state.tsdb.read(), None, &[], None);
            state.sections.write().extend(rendered);
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
            AppState::new(Tsdb::default(), registry.clone())
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
    templates: TemplateRegistry,
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
    /// Raw parquet file-level key-value metadata as a JSON object.
    file_metadata: parking_lot::RwLock<Option<String>>,
}

impl AppState {
    pub fn new(tsdb: Tsdb, templates: TemplateRegistry) -> Self {
        Self {
            sections: Default::default(),
            tsdb: Arc::new(parking_lot::RwLock::new(tsdb)),
            templates,
            snapshots: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
            live: AtomicBool::new(false),
            parquet_path: parking_lot::RwLock::new(None),
            systeminfo: parking_lot::RwLock::new(None),
            selection: parking_lot::RwLock::new(None),
            file_checksum: parking_lot::RwLock::new(None),
            file_metadata: parking_lot::RwLock::new(None),
        }
    }
}

fn extract_parquet_metadata(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
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

            // Build a JSON object from all key-value pairs.
            let mut map = serde_json::Map::new();
            for pair in kv {
                if let Some(ref val) = pair.value {
                    // Try to parse value as JSON; fall back to plain string.
                    let json_val = serde_json::from_str(val)
                        .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
                    map.insert(pair.key.clone(), json_val);
                }
            }

            // Pre-compute multi-node info so the frontend doesn't have to
            // re-parse per_source_metadata itself.
            enrich_with_multi_node_info(&mut map);

            let file_meta = serde_json::to_string(&serde_json::Value::Object(map)).ok();

            Some((sysinfo, sel, file_meta))
        })
        .unwrap_or((None, None, None))
}

/// Enrich a file-metadata JSON map with pre-computed multi-node info.
///
/// Parses `per_source_metadata` and adds:
/// - `nodes`: ordered list of node names
/// - `service_instances`: `{ service: [{id, node}, ...] }` for non-rezolus sources
/// - `node_versions`: `{ node_name: version }` for the TopNav version display
///
/// This consolidates parsing that was previously duplicated in the JS frontend.
fn enrich_with_multi_node_info(map: &mut serde_json::Map<String, serde_json::Value>) {
    let psm = match map.get("per_source_metadata").and_then(|v| v.as_object()) {
        Some(psm) => psm.clone(),
        None => return,
    };

    // Extract node list from rezolus group
    let mut nodes = Vec::new();
    let mut node_versions = serde_json::Map::new();
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (sub_key, entry) in rez_group {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
            };
            let node_name = obj.get("node").and_then(|v| v.as_str()).unwrap_or(sub_key);
            if !nodes.contains(&node_name.to_string()) {
                nodes.push(node_name.to_string());
            }
            if let Some(version) = obj.get("version").and_then(|v| v.as_str()) {
                node_versions.insert(
                    node_name.to_string(),
                    serde_json::Value::String(version.to_string()),
                );
            }
        }
    }

    // Extract service instances from non-rezolus groups
    let mut service_instances = serde_json::Map::new();
    for (source, group) in &psm {
        if source == "rezolus" {
            continue;
        }
        let group_obj = match group.as_object() {
            Some(o) => o,
            None => continue,
        };
        let mut instances = Vec::new();
        for (sub_key, entry) in group_obj {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
            };
            let instance_id = obj
                .get("instance")
                .and_then(|v| v.as_str())
                .unwrap_or(sub_key);
            let node = obj.get("node").and_then(|v| v.as_str());
            let mut inst = serde_json::Map::new();
            inst.insert(
                "id".into(),
                serde_json::Value::String(instance_id.to_string()),
            );
            inst.insert(
                "node".into(),
                node.map(|n| serde_json::Value::String(n.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            );
            instances.push(serde_json::Value::Object(inst));
        }
        if !instances.is_empty() {
            service_instances.insert(source.clone(), serde_json::Value::Array(instances));
        }
    }

    map.insert(
        "nodes".into(),
        serde_json::Value::Array(nodes.into_iter().map(serde_json::Value::String).collect()),
    );
    if !node_versions.is_empty() {
        map.insert(
            "node_versions".into(),
            serde_json::Value::Object(node_versions),
        );
    }
    if !service_instances.is_empty() {
        map.insert(
            "service_instances".into(),
            serde_json::Value::Object(service_instances),
        );
    }
}

/// Build a multi-node systeminfo JSON object from `per_source_metadata`.
///
/// Returns `Some(json_string)` when there are multiple nodes (>1), where the
/// JSON is an object keyed by node name with the systeminfo object as value.
/// Returns `None` for single-node files (the caller falls back to flat format).
fn build_multinode_systeminfo(path: &Path) -> Option<String> {
    use crate::parquet_metadata::KEY_PER_SOURCE_METADATA;
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    let f = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(f).ok()?;
    let kv = reader.metadata().file_metadata().key_value_metadata()?;

    let psm_json = kv
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_ref())?;

    let psm: serde_json::Map<String, serde_json::Value> = serde_json::from_str(psm_json).ok()?;

    let mut nodes = serde_json::Map::new();

    // per_source_metadata is nested: { "rezolus": { "node1": {...}, "node2": {...} }, ... }
    // Extract systeminfo from each rezolus node entry.
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (node_key, entry) in rez_group {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
            };
            let sysinfo_val = match obj.get("systeminfo") {
                Some(v) => v,
                None => continue,
            };
            let node_name = obj
                .get("node")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| node_key.clone());

            nodes.insert(node_name, sysinfo_val.clone());
        }
    }

    // Only return multi-node format when there are actually multiple nodes
    if nodes.len() > 1 {
        serde_json::to_string(&serde_json::Value::Object(nodes)).ok()
    } else {
        None
    }
}

/// Extract service extension metadata from a parquet file.
///
/// Checks in order:
/// 1. Top-level `service_queries` key (single-source annotated files)
/// 2. `per_source_metadata.<source>.service_queries` (combined files)
/// 3. Built-in template for known sources (fallback)
fn extract_service_extension_metadata(
    path: &Path,
    registry: &TemplateRegistry,
) -> Vec<(String, ServiceExtension)> {
    use crate::parquet_metadata::{
        KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, NESTED_SERVICE_QUERIES,
    };
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    let mut results = Vec::new();

    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return results,
    };
    let reader = match SerializedFileReader::new(f) {
        Ok(r) => r,
        Err(_) => return results,
    };
    let kv = match reader.metadata().file_metadata().key_value_metadata() {
        Some(kv) => kv,
        None => return results,
    };

    // 1. Top-level service_queries (written by `parquet annotate`).
    if let Some(sq_json) = kv
        .iter()
        .find(|kv| kv.key == KEY_SERVICE_QUERIES)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(ext) = serde_json::from_str::<ServiceExtension>(sq_json) {
            let source = kv
                .iter()
                .find(|kv| kv.key == KEY_SOURCE)
                .and_then(|kv| kv.value.as_deref())
                .unwrap_or(&ext.service_name);
            results.push((source.to_string(), ext));
        }
    }

    // 2. Nested under per_source_metadata (combined files).
    if let Some(metadata_json) = kv
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(metadata_map) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(metadata_json)
        {
            // per_source_metadata is nested: { "source": { "id": { ... }, ... }, ... }
            // Check each source group's entries for service_queries.
            for (source, group_val) in &metadata_map {
                // Skip sources already found via top-level service_queries.
                if results.iter().any(|(s, _)| s == source) {
                    continue;
                }
                if let Some(group) = group_val.as_object() {
                    for (_sub_key, entry) in group {
                        if let Some(sq) = entry.get(NESTED_SERVICE_QUERIES) {
                            if let Ok(ext) = serde_json::from_value::<ServiceExtension>(sq.clone())
                            {
                                results.push((source.clone(), ext));
                                break; // one extension per source
                            }
                        }
                    }
                }
            }

            // 3a. No service_queries found for a source — check built-in templates.
            for source in metadata_map.keys() {
                if results.iter().any(|(s, _)| s == source) {
                    continue;
                }
                if let Some(ext) = registry.get(source) {
                    results.push((source.clone(), ext.clone()));
                }
            }
        }
    }

    // 3b. No per_source_metadata — check the top-level source key for a template.
    if results.is_empty() {
        if let Some(source) = kv
            .iter()
            .find(|kv| kv.key == KEY_SOURCE)
            .and_then(|kv| kv.value.as_deref())
        {
            if let Some(ext) = registry.get(source) {
                results.push((source.to_string(), ext.clone()));
            }
        }
    }

    results
}

/// Validate KPI availability for service extensions by running each KPI's
/// PromQL query against the loaded TSDB. Sets `available = false` on KPIs
/// whose queries return empty results (e.g. zero-traffic histograms).
fn validate_service_extensions(
    tsdb: &std::sync::Arc<Tsdb>,
    exts: &mut [(String, ServiceExtension)],
) {
    let engine = QueryEngine::new(tsdb.clone());
    let (start, end) = engine.get_time_range();

    for (_source, ext) in exts.iter_mut() {
        for kpi in &mut ext.kpis {
            let query = kpi.effective_query();
            let has_data = match engine.query_range(&query, start, end, 1.0) {
                Ok(result) => match &result {
                    promql::QueryResult::Vector { result } => !result.is_empty(),
                    promql::QueryResult::Matrix { result } => !result.is_empty(),
                    promql::QueryResult::Scalar { .. } => true,
                    promql::QueryResult::HistogramHeatmap { result } => !result.data.is_empty(),
                },
                Err(_) => false,
            };
            kpi.available = has_data;
        }
    }
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
        .route("/file_metadata", get(file_metadata_handler))
        .route(
            "/upload",
            axum::routing::post(upload_parquet)
                .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
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
        .route("/sitemap", get(sitemap))
        .route("/data/{*path}", get(data))
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

/// Shared HTML head boilerplate for standalone pages (about, sitemap).
/// Reads `rezolus-theme` from localStorage to match the viewer's theme choice.
const STANDALONE_HEAD: &str = r#"<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<link rel="preconnect" href="https://fonts.googleapis.com"/>
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin/>
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;600&display=swap" rel="stylesheet"/>
<script>
// Apply saved theme before first paint to avoid flash
(function(){
  var t = localStorage.getItem('rezolus-theme');
  if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
})();
</script>
<style>
/* Dark (default) */
:root {
  --bg: #0a0e14;
  --bg-card: #0d1117;
  --border-subtle: rgba(48,54,61,0.4);
  --fg: #e6edf3;
  --fg-secondary: #8b949e;
  --accent: #58a6ff;
}
/* Light — matches viewer's [data-theme="light"] */
[data-theme="light"] {
  --bg: #f6f8fa;
  --bg-card: #ffffff;
  --border-subtle: rgba(0,0,0,0.1);
  --fg: #1f2328;
  --fg-secondary: #636c76;
  --accent: #0969da;
}
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: 'Inter', -apple-system, sans-serif;
  background: var(--bg);
  color: var(--fg);
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  padding: 2rem;
}
.card {
  background: var(--bg-card);
  border: 1px solid var(--border-subtle);
  border-radius: 12px;
  padding: 2rem 2.5rem;
  max-width: 600px;
  width: 100%;
  text-align: center;
}
a { color: var(--accent); text-decoration: none; transition: border-color 0.15s; }
a:hover { opacity: 0.85; }
code {
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.8rem;
  color: var(--accent);
}
</style>"#;

// Styled /about page handler
async fn about() -> axum::response::Html<String> {
    let version = env!("CARGO_PKG_VERSION");
    axum::response::Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<title>Rezolus — About</title>
{STANDALONE_HEAD}
<style>
h1 {{ font-size: 1.75rem; font-weight: 700; margin-bottom: 0.25rem; }}
.version {{
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.85rem;
  color: var(--accent);
  margin-bottom: 1.5rem;
}}
.links {{ display: flex; gap: 1rem; justify-content: center; flex-wrap: wrap; }}
.links a {{
  font-size: 0.9rem;
  padding: 0.4rem 0.8rem;
  border: 1px solid var(--border-subtle);
  border-radius: 6px;
}}
.links a:hover {{ border-color: var(--accent); }}
p {{
  color: var(--fg-secondary);
  font-size: 0.9rem;
  margin-bottom: 1.25rem;
  line-height: 1.5;
}}
</style>
</head>
<body>
<div class="card">
  <h1>Rezolus</h1>
  <div class="version">v{version}</div>
  <p>High-resolution systems performance telemetry agent with eBPF instrumentation.</p>
  <div class="links">
    <a href="https://rezolus.com">Website</a>
    <a href="https://github.com/iopsystems/rezolus">GitHub</a>
    <a href="/sitemap">Sitemap</a>
  </div>
</div>
</body>
</html>"#
    ))
}

/// Lists all endpoints served by the viewer (max 2 levels deep).
async fn sitemap() -> axum::response::Html<String> {
    let routes = [
        ("/", "Dashboard"),
        ("/about", "About page"),
        ("/sitemap", "This page"),
        ("/data/{path}", "Dashboard section data"),
        ("/api/v1/mode", "Viewer mode (loaded/live)"),
        ("/api/v1/query", "PromQL instant query"),
        ("/api/v1/query_range", "PromQL range query"),
        ("/api/v1/labels", "Label names"),
        ("/api/v1/label/{name}/values", "Label values"),
        ("/api/v1/metadata", "File checksum"),
        ("/api/v1/systeminfo", "System information"),
        ("/api/v1/selection", "Saved selection state"),
        ("/api/v1/file_metadata", "Parquet file metadata"),
        ("/api/v1/reset", "Reset TSDB data"),
        ("/api/v1/save", "Download parquet"),
        ("/api/v1/upload", "Upload parquet"),
        ("/api/v1/connect", "Connect to live agent"),
        ("/api/v1/save_with_selection", "Save with selection state"),
    ];

    let rows: Vec<String> = routes
        .iter()
        .map(|(path, desc)| format!(r#"<tr><td><code>{path}</code></td><td>{desc}</td></tr>"#))
        .collect();

    axum::response::Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<title>Rezolus — Sitemap</title>
{STANDALONE_HEAD}
<style>
.card {{ text-align: left; }}
h1 {{ font-size: 1.5rem; font-weight: 700; margin-bottom: 1.25rem; }}
table {{ width: 100%; border-collapse: collapse; }}
tr {{ border-bottom: 1px solid var(--border-subtle); }}
tr:last-child {{ border-bottom: none; }}
td {{ padding: 0.5rem 0; font-size: 0.85rem; vertical-align: top; }}
td:first-child {{ white-space: nowrap; padding-right: 1.5rem; }}
td:last-child {{ color: var(--fg-secondary); }}
.back {{
  color: var(--fg-secondary);
  font-size: 0.85rem;
  margin-top: 1.25rem;
  display: inline-block;
}}
.back:hover {{ color: var(--accent); }}
</style>
</head>
<body>
<div class="card">
  <h1>Endpoints</h1>
  <table>{}</table>
  <a class="back" href="/about">&larr; About</a>
</div>
</body>
</html>"#,
        rows.join("")
    ))
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

async fn file_metadata_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match &*state.file_metadata.read() {
        Some(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json.clone(),
        )
            .into_response(),
        None => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            "{}".to_string(),
        )
            .into_response(),
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

    let mut service_exts = extract_service_extension_metadata(&temp_path, &state.templates);
    let data_arc = std::sync::Arc::new(data);
    validate_service_extensions(&data_arc, &mut service_exts);
    let data = std::sync::Arc::try_unwrap(data_arc)
        .ok()
        .expect("Arc still shared");
    let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
    let rendered = dashboard::dashboard::generate(&data, filesize, &service_refs, None);
    let (systeminfo, selection, file_meta) = extract_parquet_metadata(&temp_path);
    let file_checksum = compute_file_checksum(&temp_path);

    {
        let mut tsdb = state.tsdb.write();
        *tsdb = data;
    }
    {
        let mut sections = state.sections.write();
        *sections = rendered;
    }
    let multinode_sysinfo = build_multinode_systeminfo(&temp_path);
    *state.parquet_path.write() = Some(temp_path);
    *state.systeminfo.write() = multinode_sysinfo.or(systeminfo);
    *state.selection.write() = selection;
    *state.file_checksum.write() = file_checksum;
    *state.file_metadata.write() = file_meta;

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
    let rendered = dashboard::dashboard::generate(&tsdb, None, &[], None);

    // Update shared state
    {
        let mut db = state.tsdb.write();
        *db = tsdb;
    }
    {
        let mut sections = state.sections.write();
        *sections = rendered;
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
            use parquet::file::metadata::KeyValue;

            let mut kv_meta =
                crate::parquet_tools::read_file_metadata(&path).map_err(|e| e.to_string())?;
            kv_meta.retain(|kv| kv.key != "selection");
            kv_meta.push(KeyValue {
                key: "selection".to_string(),
                value: Some(selection_json),
            });

            let output = crate::parquet_tools::rewrite_parquet(&path, kv_meta, None)
                .map_err(|e| e.to_string())?;
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
