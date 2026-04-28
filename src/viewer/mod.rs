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

/// Shared entry point for loading the template registry. Both the
/// viewer (`rezolus view`) and `rezolus parquet annotate/filter` call
/// this. Precedence: explicit `--templates <path>` > env var /
/// `config/templates/` default (developer-mode or explicit path only).
/// Release builds fall back to templates baked into the binary via
/// `include_dir!`; developer-mode continues to read from disk so
/// template edits don't require a rebuild.
pub fn load_template_registry(cli_path: Option<&Path>) -> TemplateRegistry {
    if cli_path.is_some() {
        return TemplateRegistry::resolve_and_load(cli_path);
    }
    #[cfg(not(feature = "developer-mode"))]
    {
        TemplateRegistry::from_embedded(&crate::EMBEDDED_TEMPLATES).unwrap_or_else(|e| {
            warn!("failed to parse embedded templates: {e}");
            TemplateRegistry::empty()
        })
    }
    #[cfg(feature = "developer-mode")]
    {
        TemplateRegistry::resolve_and_load(None)
    }
}

#[cfg(test)]
pub use dashboard::Kpi;
pub use dashboard::{CategoryExtension, ServiceExtension, TemplateRegistry};

// Re-export from metriken-query crate
pub use metriken_query::promql;
pub use metriken_query::tsdb;

use promql::QueryEngine;
use tsdb::*;

pub mod capture_registry;

use capture_registry::{CaptureId, CaptureRegistry};

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
                .help(
                    "First capture: parquet file, agent URL (http://…), or \
                     alias=parquet (e.g. redis=./a.parquet). The alias is a \
                     display label; internal identifiers stay baseline/experiment.",
                )
                .action(clap::ArgAction::Set)
                .required(false)
                .index(1),
        )
        .arg(
            clap::Arg::new("EXPERIMENT")
                .help(
                    "Optional second capture for A/B comparison: parquet path \
                     or alias=parquet (e.g. valkey=./b.parquet).",
                )
                .action(clap::ArgAction::Set)
                .required(false)
                .index(2),
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
                .long("listen")
                .short('l')
                .value_name("ADDR")
                .help("Viewer listen address (e.g. 127.0.0.1:8080)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(SocketAddr)),
        )
        .arg(
            clap::Arg::new("templates")
                .long("templates")
                .value_name("DIR")
                .help("Directory containing service extension template JSON files")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("CATEGORY")
                .long("category")
                .value_name("NAME")
                .help(
                    "Activate category mode using the named category template \
                     (e.g. `inference-library`). Each capture's CLI alias must \
                     appear in the category template's `members` list.",
                )
                .action(clap::ArgAction::Set),
        )
}

pub struct Config {
    source: Source,
    experiment_path: Option<PathBuf>,
    baseline_alias: Option<String>,
    experiment_alias: Option<String>,
    category_name: Option<String>,
    verbose: u8,
    listen: SocketAddr,
    templates_dir: Option<PathBuf>,
}

/// Split a positional input into an optional alias and the remaining
/// path-or-url. The alias prefix must look "identifier-like" — no
/// path separators, no colon (which would collide with URL schemes),
/// no whitespace. Anything else parses as a bare path.
///
/// Examples:
///   "redis=./a.parquet"          → (Some("redis"), "./a.parquet")
///   "./a.parquet"                → (None,          "./a.parquet")
///   "http://localhost:4241"      → (None,          "http://localhost:4241")  (colon guard)
///   "/abs/path=weird.parquet"    → (None,          "/abs/path=weird.parquet") (slash guard)
fn split_alias(raw: &str) -> (Option<String>, &str) {
    if let Some(eq) = raw.find('=') {
        let (lhs, rest) = raw.split_at(eq);
        let rhs = &rest[1..]; // skip the '=' itself
        let lhs_ok = !lhs.is_empty()
            && !lhs.contains('/')
            && !lhs.contains('\\')
            && !lhs.contains(':')
            && !lhs.contains(char::is_whitespace);
        if lhs_ok {
            return (Some(lhs.to_string()), rhs);
        }
    }
    (None, raw)
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        let (baseline_alias, source) = match args.get_one::<String>("INPUT") {
            Some(raw) => {
                let (alias, body) = split_alias(raw);
                let source = if body.starts_with("http://") || body.starts_with("https://") {
                    Source::Live(
                        body.parse::<Url>()
                            .map_err(|e| format!("invalid URL: {e}"))?,
                    )
                } else {
                    Source::File(PathBuf::from(body))
                };
                (alias, source)
            }
            None => (None, Source::Empty),
        };

        let (experiment_alias, experiment_path) = match args.get_one::<String>("EXPERIMENT") {
            Some(raw) => {
                let (alias, body) = split_alias(raw);
                (alias, Some(PathBuf::from(body)))
            }
            None => (None, None),
        };

        Ok(Config {
            source,
            experiment_path,
            baseline_alias,
            experiment_alias,
            category_name: args.get_one::<String>("CATEGORY").cloned(),
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

    let registry = load_template_registry(config.templates_dir.as_deref());

    let state = match &config.source {
        Source::File(path) => {
            info!("Loading data from parquet file...");
            let data = Tsdb::load(path)
                .map_err(|e| {
                    eprintln!("failed to load data from parquet: {e}");
                    std::process::exit(1);
                })
                .unwrap();

            let (systeminfo, selection, file_meta) = extract_parquet_metadata(path);
            // CLI startup log only — the canonical extraction with
            // alias-aware lookup happens inside `regenerate_dashboards`
            // below. Pass the baseline alias here too so the startup
            // log reports what the dashboard will actually use.
            let mut service_exts = extract_service_extension_metadata(
                path,
                &registry,
                config.baseline_alias.as_deref(),
            );

            // Validate KPI availability against the loaded data so that
            // template-derived dashboards hide KPIs with no data. The
            // `service_exts` here is consumed only for the startup log
            // below; `regenerate_dashboards` re-extracts and re-validates
            // per-capture once the experiment (if any) has been attached.
            validate_service_extensions(&data, &mut service_exts);

            // Compute SHA-256 checksum of the parquet data (excluding footer metadata).
            // Parquet layout: [magic 4B] [row groups...] [footer] [footer_len 4B] [magic 4B]
            // We hash only [0, file_size - 8 - footer_len) so the checksum is stable
            // regardless of key-value metadata changes (e.g. selection annotations).
            info!("Computing file checksum...");
            let file_checksum = compute_file_checksum(path);

            if service_exts.is_empty()
                && (config.baseline_alias.is_some() || config.category_name.is_some())
            {
                warn!(
                    "no service extension matched baseline alias {:?} or its parquet source",
                    config.baseline_alias
                );
            }
            for (source, ext) in &service_exts {
                let available = ext.kpis.iter().filter(|k| k.available).count();
                info!(
                    "Found service extension for {:?} from source {:?} ({}/{} KPIs available) — baseline",
                    ext.service_name,
                    source,
                    available,
                    ext.kpis.len()
                );
            }

            let state = AppState::new(data, registry.clone());
            *state.parquet_path.write() = Some(path.clone());
            let multinode_sysinfo = build_multinode_systeminfo(path);
            state
                .captures
                .set_baseline_systeminfo(multinode_sysinfo.or(systeminfo));
            *state.selection.write() = selection;
            *state.file_checksum.write() = file_checksum;
            state.captures.set_baseline_file_metadata(file_meta);
            state
                .captures
                .set_baseline_alias(config.baseline_alias.clone());

            // --category requires both captures to carry CLI aliases AND
            // each alias to appear in the category template's members
            // list. Refuse to launch on misconfiguration; silently
            // falling back hides user intent and produces a confusing
            // dashboard. The check is bundled here at startup so the
            // user finds out before the browser opens.
            if let Some(ref cat_name) = config.category_name {
                let category = registry.get_category(cat_name).unwrap_or_else(|| {
                    eprintln!("no category template named {cat_name:?} found in the registry");
                    std::process::exit(1);
                });
                let baseline_alias = config.baseline_alias.as_deref().unwrap_or_else(|| {
                    eprintln!(
                        "--category {cat_name:?} requires the baseline capture to have an alias (e.g. `vllm=path.parquet`)"
                    );
                    std::process::exit(1);
                });
                let experiment_alias = config.experiment_alias.as_deref().unwrap_or_else(|| {
                    eprintln!(
                        "--category {cat_name:?} requires the experiment capture to have an alias (e.g. `sglang=path.parquet`)"
                    );
                    std::process::exit(1);
                });
                for alias in [baseline_alias, experiment_alias] {
                    if !category.members.iter().any(|m| m == alias) {
                        eprintln!(
                            "alias {alias:?} is not a member of category {cat_name:?} (members: {:?})",
                            category.members
                        );
                        std::process::exit(1);
                    }
                }
                info!(
                    "Activated category {:?} (members: {:?}) — baseline {:?}, experiment {:?}",
                    cat_name, category.members, baseline_alias, experiment_alias,
                );
                *state.category_name.write() = Some(cat_name.clone());
            }

            // Attach the optional experiment capture for A/B comparison.
            // The experiment path is recorded in `cli_experiment_path`
            // (NOT `experiment_parquet_path`). That separation
            // matters: `experiment_parquet_path` is for server-owned
            // temp files written by the HTTP attach handler so they
            // can be cleaned up on detach. The CLI path is the user's
            // own file on disk — detaching must not delete it.
            // `regenerate_dashboards` consults both fields.
            if let Some(exp_path) = &config.experiment_path {
                info!("Loading experiment from parquet file...");
                let (exp_sysinfo, _exp_selection, exp_file_meta) =
                    extract_parquet_metadata(exp_path);
                match Tsdb::load(exp_path) {
                    Ok(mut exp_tsdb) => {
                        // Stamp the Tsdb with its filename (basename) so
                        // the viewer can surface it in the compare badge.
                        let base = exp_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("experiment.parquet")
                            .to_string();
                        exp_tsdb.set_filename(base);
                        state.captures.attach_experiment(
                            exp_tsdb,
                            exp_sysinfo,
                            exp_file_meta,
                            config.experiment_alias.clone(),
                        );
                        *state.cli_experiment_path.write() = Some(exp_path.clone());
                        info!("Attached experiment capture: {}", exp_path.display());

                        // Mirror the baseline log line for the
                        // experiment so the user can see whether the
                        // alias-keyed template lookup succeeded.
                        let mut exp_exts = extract_service_extension_metadata(
                            exp_path,
                            &registry,
                            config.experiment_alias.as_deref(),
                        );
                        if let Some(handle) = state.captures.get(CaptureId::Experiment) {
                            let exp_data = handle.read();
                            validate_service_extensions(&exp_data, &mut exp_exts);
                        }
                        if exp_exts.is_empty() {
                            warn!(
                                "no service extension matched experiment alias {:?} or its parquet source",
                                config.experiment_alias
                            );
                        }
                        for (source, ext) in &exp_exts {
                            let available = ext.kpis.iter().filter(|k| k.available).count();
                            info!(
                                "Found service extension for {:?} from source {:?} ({}/{} KPIs available) — experiment",
                                ext.service_name,
                                source,
                                available,
                                ext.kpis.len()
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "failed to load experiment '{}': {e}. Starting in single-capture mode.",
                            exp_path.display(),
                        );
                    }
                }
            }

            // Now that BOTH captures (baseline + optional experiment)
            // are attached, generate the dashboard once. This is the
            // single place that picks up a category when the registry
            // has one whose members match the two service extensions.
            info!("Generating dashboards...");
            regenerate_dashboards(&state);

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
            let rendered = dashboard::dashboard::generate(
                &state.baseline_tsdb().read(),
                None,
                &[],
                None,
                None,
            );
            state.sections.write().extend(rendered);
            state.live.store(true, Ordering::Relaxed);

            state.captures.set_baseline_systeminfo(agent_systeminfo);

            // Spawn the ingest loop
            let ingest_tsdb = state.baseline_tsdb();
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

    // The experiment CLI arg is only honored when the baseline is a parquet
    // file. Live and upload-only modes manage the experiment slot via the
    // HTTP attach endpoint instead.
    if config.experiment_path.is_some() && !matches!(config.source, Source::File(_)) {
        warn!(
            "--experiment ignored outside of file mode (v1 compare requires a baseline parquet file)"
        );
    }

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
    /// Per-capture TSDB + metadata. Single-capture callers always target
    /// `CaptureId::Baseline`; the experiment slot is empty unless a compare
    /// mode hand-off has attached one.
    captures: Arc<CaptureRegistry>,
    templates: TemplateRegistry,
    /// Raw msgpack snapshot bytes for parquet export (live mode only).
    snapshots: Arc<parking_lot::Mutex<VecDeque<Vec<u8>>>>,
    live: AtomicBool,
    /// Original parquet file path (file mode only).
    parquet_path: parking_lot::RwLock<Option<std::path::PathBuf>>,
    /// Temp parquet path for the attached experiment capture (cleared on detach).
    /// Populated by the HTTP attach handler, which owns the temp file and
    /// deletes it on detach. The CLI startup path uses `cli_experiment_path`
    /// instead so detach never touches the user's own file.
    experiment_parquet_path: parking_lot::RwLock<Option<std::path::PathBuf>>,
    /// User-supplied experiment parquet path from the CLI (e.g.
    /// `rezolus view a.parquet b.parquet`). Read-only — not deleted on
    /// detach. Detach only clears `experiment_parquet_path`. Kept
    /// separate from that field so `regenerate_dashboards` can find
    /// the experiment metadata without risking the user's file.
    cli_experiment_path: parking_lot::RwLock<Option<std::path::PathBuf>>,
    /// Active category template name (when `--category` was supplied at
    /// startup). `regenerate_dashboards` resolves this against the
    /// registry on every regen and validates the attached aliases
    /// against the category's `members` list. None = no category mode.
    category_name: parking_lot::RwLock<Option<String>>,
    /// Serialized selection JSON from parquet metadata.
    selection: parking_lot::RwLock<Option<String>>,
    /// SHA-256 hex digest of the source parquet file (file mode only).
    file_checksum: parking_lot::RwLock<Option<String>>,
}

impl AppState {
    pub fn new(tsdb: Tsdb, templates: TemplateRegistry) -> Self {
        Self {
            sections: Default::default(),
            captures: Arc::new(CaptureRegistry::new(tsdb, None, None, None)),
            templates,
            snapshots: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
            live: AtomicBool::new(false),
            parquet_path: parking_lot::RwLock::new(None),
            experiment_parquet_path: parking_lot::RwLock::new(None),
            cli_experiment_path: parking_lot::RwLock::new(None),
            category_name: parking_lot::RwLock::new(None),
            selection: parking_lot::RwLock::new(None),
            file_checksum: parking_lot::RwLock::new(None),
        }
    }

    /// Shorthand for the baseline TSDB handle. Every existing caller that
    /// used to dereference `state.tsdb` directly lands on baseline — the
    /// registry guarantees the baseline slot is always present.
    fn baseline_tsdb(&self) -> Arc<parking_lot::RwLock<Tsdb>> {
        self.captures
            .get(CaptureId::Baseline)
            .expect("baseline capture is always present")
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

/// Resolve the active category for a regen pass. Activation requires:
/// `state.category_name` is Some, the named category exists in the
/// registry, exactly two service refs are attached, and each ref's
/// source name (== CLI alias when one was provided) appears in the
/// category's `members` list. Returns None when any of those fail —
/// the caller falls back to per-member rendering. CLI startup ran
/// stricter checks, so silent fall-back here only happens at runtime
/// (e.g. mid-session experiment detach) and that's the correct UX.
fn lookup_category<'a>(
    state: &AppState,
    registry: &'a TemplateRegistry,
    service_refs: &[(&str, &ServiceExtension)],
) -> Option<(&'a str, &'a CategoryExtension)> {
    let cat_name = state.category_name.read().clone()?;
    if service_refs.len() != 2 {
        return None;
    }
    let category = registry.get_category(&cat_name)?;
    for (alias, _) in service_refs {
        if !category.members.iter().any(|m| m == alias) {
            return None;
        }
    }
    Some((category.service_name.as_str(), category))
}

/// Regenerate the dashboard sections from the currently attached
/// captures. Pulls service extensions from each capture's parquet
/// metadata, validates them against the live tsdb data, looks up a
/// category in the registry when both captures are present, and renders
/// the resulting section map into `state.sections`. Called at CLI
/// startup after the experiment attaches, and on every HTTP attach /
/// detach so the section list stays in sync with which captures are
/// loaded.
fn regenerate_dashboards(state: &AppState) {
    let registry = &state.templates;
    let baseline_path = state.parquet_path.read().clone();
    // Prefer the HTTP-owned temp path; fall back to the CLI-supplied
    // user path. The two are stored in separate fields so that
    // `detach_experiment` can safely delete only server-owned temp
    // files — see `cli_experiment_path` for the rationale.
    let experiment_path = state
        .experiment_parquet_path
        .read()
        .clone()
        .or_else(|| state.cli_experiment_path.read().clone());

    // Extract service extensions per-capture; baseline-source exts
    // validate against the baseline tsdb, experiment-source exts
    // against the experiment tsdb, so a KPI present only in one
    // recording isn't wrongly marked unavailable.
    //
    // When the CLI provided an alias, the alias overrides whatever
    // `source` the parquet's metadata carries. That alias is also the
    // member key used to verify against the active category template.
    let baseline_alias = state.captures.alias(CaptureId::Baseline);
    let experiment_alias = state.captures.alias(CaptureId::Experiment);
    let mut baseline_exts: Vec<(String, ServiceExtension)> = baseline_path
        .as_ref()
        .map(|p| extract_service_extension_metadata(p, registry, baseline_alias.as_deref()))
        .unwrap_or_default();
    let mut experiment_exts: Vec<(String, ServiceExtension)> = experiment_path
        .as_ref()
        .map(|p| extract_service_extension_metadata(p, registry, experiment_alias.as_deref()))
        .unwrap_or_default();

    {
        let baseline_handle = state.baseline_tsdb();
        let baseline_data = baseline_handle.read();
        validate_service_extensions(&baseline_data, &mut baseline_exts);
    }
    if !experiment_exts.is_empty() {
        if let Some(experiment_handle) = state.captures.get(CaptureId::Experiment) {
            let experiment_data = experiment_handle.read();
            validate_service_extensions(&experiment_data, &mut experiment_exts);
        }
    }

    let mut service_exts = baseline_exts;
    service_exts.extend(experiment_exts);

    let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
    let category = lookup_category(state, registry, &service_refs);

    let filesize = baseline_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));

    let rendered = dashboard::dashboard::generate(
        &state.baseline_tsdb().read(),
        filesize,
        &service_refs,
        category,
        None,
    );

    let mut sections = state.sections.write();
    *sections = rendered;
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
    alias: Option<&str>,
) -> Vec<(String, ServiceExtension)> {
    use crate::parquet_metadata::{
        KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, NESTED_SERVICE_QUERIES,
    };
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    // When the user provided a CLI alias, that alias is the source of
    // truth for template lookup AND for the returned source key (the
    // key drives category member matching downstream). The parquet's
    // own `source`/`service_queries` are ignored — useful when a
    // capture was produced by a generic benchmarking tool whose source
    // metadata doesn't match a service template name.
    if let Some(alias) = alias {
        if let Some(ext) = registry.get(alias) {
            return vec![(alias.to_string(), ext.clone())];
        }
        // Alias supplied but no matching template — leave results empty
        // so `lookup_category`'s membership check fails cleanly.
        return Vec::new();
    }

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
fn validate_service_extensions(tsdb: &Tsdb, exts: &mut [(String, ServiceExtension)]) {
    let engine = QueryEngine::new(tsdb);
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
        .route(
            "/captures/experiment",
            axum::routing::post(attach_experiment)
                .delete(detach_experiment)
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

/// Shared HTML head for standalone pages — reuses the main viewer stylesheet
/// and applies the saved theme before first paint.
const STANDALONE_HEAD: &str = r#"<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<script>!function(){var t=localStorage.getItem('rezolus-theme');if(t==='light'||t==='dark')document.documentElement.setAttribute('data-theme',t)}()</script>
<link rel="stylesheet" href="/lib/style.css"/>
<style>body{display:flex;align-items:center;justify-content:center;padding:2rem}</style>"#;

// Styled /about page handler
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
        "compare_mode": state.captures.experiment_attached(),
        "category": state.category_name.read().clone(),
    }))
}

/// Query param for endpoints that select between baseline and experiment.
#[derive(serde::Deserialize)]
struct CaptureParam {
    #[serde(default)]
    capture: Option<String>,
}

impl CaptureParam {
    fn capture_id(&self) -> CaptureId {
        CaptureId::parse_opt(self.capture.as_deref())
    }
}

/// Returns the system hardware summary from parquet metadata or the live system.
async fn systeminfo_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Query(p): axum::extract::Query<CaptureParam>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

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
    axum::extract::Query(p): axum::extract::Query<CaptureParam>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match state.captures.file_metadata(p.capture_id()) {
        Some(json) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json,
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
    let capture = CaptureId::parse_opt(params.capture.as_deref());
    let tsdb_handle = match state.captures.get(capture) {
        Some(t) => t,
        None => {
            return axum::response::Json(ApiResponse::error(
                format!("capture '{:?}' not attached", capture),
                "capture_not_found".to_string(),
            ));
        }
    };
    let tsdb = tsdb_handle.read();
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
    let capture = CaptureId::parse_opt(params.capture.as_deref());
    let tsdb_handle = match state.captures.get(capture) {
        Some(t) => t,
        None => {
            return axum::response::Json(ApiResponse::error(
                format!("capture '{:?}' not attached", capture),
                "capture_not_found".to_string(),
            ));
        }
    };
    let tsdb = tsdb_handle.read();
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
    axum::extract::Query(p): axum::extract::Query<CaptureParam>,
) -> axum::response::Json<ApiResponse<serde_json::Value>> {
    let capture = p.capture_id();
    let tsdb_handle = match state.captures.get(capture) {
        Some(t) => t,
        None => {
            return axum::response::Json(ApiResponse::error(
                format!("capture {:?} not attached", capture),
                "capture_not_found".to_string(),
            ));
        }
    };
    let tsdb = tsdb_handle.read();
    let engine = QueryEngine::new(&*tsdb);
    let time_range = engine.get_time_range();
    let mut metadata = serde_json::json!({
        "minTime": time_range.0,
        "maxTime": time_range.1,
        "filename": tsdb.filename(),
    });
    // Display alias, when the CLI provided one (e.g. `redis=./a.parquet`).
    // Present to the frontend as `alias` alongside `filename`; absent
    // when no alias was set, so the UI falls back to the capture id.
    if let Some(alias) = state.captures.alias(capture) {
        metadata["alias"] = serde_json::json!(alias);
    }
    // File checksum is only tracked for the baseline capture today.
    if matches!(capture, capture_registry::CaptureId::Baseline) {
        if let Some(checksum) = &*state.file_checksum.read() {
            metadata["fileChecksum"] = serde_json::json!(checksum);
        }
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

    // HTTP upload path has no alias plumbing (yet); template lookup
    // falls back to the parquet's embedded `source` metadata.
    let mut service_exts = extract_service_extension_metadata(&temp_path, &state.templates, None);
    validate_service_extensions(&data, &mut service_exts);
    let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
    let rendered = dashboard::dashboard::generate(&data, filesize, &service_refs, None, None);
    let (systeminfo, selection, file_meta) = extract_parquet_metadata(&temp_path);
    let file_checksum = compute_file_checksum(&temp_path);

    {
        let tsdb_handle = state.baseline_tsdb();
        let mut tsdb = tsdb_handle.write();
        *tsdb = data;
    }
    {
        let mut sections = state.sections.write();
        *sections = rendered;
    }
    let multinode_sysinfo = build_multinode_systeminfo(&temp_path);
    *state.parquet_path.write() = Some(temp_path);
    state
        .captures
        .set_baseline_systeminfo(multinode_sysinfo.or(systeminfo));
    *state.selection.write() = selection;
    *state.file_checksum.write() = file_checksum;
    state.captures.set_baseline_file_metadata(file_meta);

    axum::response::Json(ApiResponse::success(serde_json::json!({
        "filename": filename,
    })))
}

/// Attach an experiment parquet for A/B comparison. Body is raw parquet bytes.
/// Returns 409 if an experiment is already attached (caller must DELETE first).
async fn attach_experiment(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    if state.captures.experiment_attached() {
        return (
            StatusCode::CONFLICT,
            "experiment already attached; DELETE first",
        )
            .into_response();
    }

    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing parquet bytes").into_response();
    }

    let filename = headers
        .get("x-rezolus-filename")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
        .unwrap_or_else(|| "experiment.parquet".to_string());

    let temp_path =
        std::env::temp_dir().join(format!("rezolus-experiment-{}.parquet", std::process::id(),));
    if let Err(e) = std::fs::write(&temp_path, &body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store upload: {e}"),
        )
            .into_response();
    }

    let mut tsdb = match Tsdb::load(&temp_path) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            return (
                StatusCode::BAD_REQUEST,
                format!("failed to load parquet: {e}"),
            )
                .into_response();
        }
    };
    // Stamp with the uploader's filename so the viewer can display it
    // in the compare badge.
    tsdb.set_filename(filename);

    let (sysinfo, _selection, file_meta) = extract_parquet_metadata(&temp_path);
    // HTTP-attached experiments don't carry an alias — aliases only
    // come in via the CLI today. Keep None for now; this parameter is
    // here so a future `x-rezolus-alias` upload header can thread one
    // through without further signature changes.
    state
        .captures
        .attach_experiment(tsdb, sysinfo.clone(), file_meta, None);
    *state.experiment_parquet_path.write() = Some(temp_path);

    // Rebuild the section map now that both captures are present.
    // Picks up a category when the registry has one whose members match
    // the two service extensions.
    regenerate_dashboards(&state);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        sysinfo.unwrap_or_else(|| "{}".into()),
    )
        .into_response()
}

/// Detach the currently attached experiment (if any) and clean up its temp file.
async fn detach_experiment(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    state.captures.detach_experiment();
    if let Some(path) = state.experiment_parquet_path.write().take() {
        let _ = std::fs::remove_file(&path);
    }
    // Also clear the CLI-supplied experiment path so the section
    // regeneration below doesn't rebuild the category against a
    // detached capture. Note: we only clear the path reference, not
    // the file itself — the user's parquet on disk is left alone.
    state.cli_experiment_path.write().take();

    // Rebuild the section map back to baseline-only (drops category /
    // experiment service section).
    regenerate_dashboards(&state);

    StatusCode::OK.into_response()
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
    let rendered = dashboard::dashboard::generate(&tsdb, None, &[], None, None);

    // Update shared state
    {
        let tsdb_handle = state.baseline_tsdb();
        let mut db = tsdb_handle.write();
        *db = tsdb;
    }
    {
        let mut sections = state.sections.write();
        *sections = rendered;
    }
    state.captures.set_baseline_systeminfo(agent_systeminfo);
    state.live.store(true, Ordering::Relaxed);

    // Spawn the ingest loop
    let ingest_tsdb = state.baseline_tsdb();
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
    let tsdb_handle = state.baseline_tsdb();
    let (source, version, filename) = {
        let tsdb = tsdb_handle.read();
        (
            tsdb.source().to_string(),
            tsdb.version().to_string(),
            tsdb.filename().to_string(),
        )
    };

    {
        let mut tsdb = tsdb_handle.write();
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
    let sysinfo_json = state.captures.systeminfo(CaptureId::Baseline);

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

    let sysinfo_json = state.captures.systeminfo(CaptureId::Baseline);

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
