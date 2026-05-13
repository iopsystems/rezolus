//! Viewer subcommand: serves a web dashboard backed by parquet files,
//! a live agent connection, or upload-only mode.
//!
//! Submodules:
//! - [`state`] — `AppState`, `LazySectionStore`, `CaptureParam`, API
//!   envelope types
//! - [`metadata`] — parquet metadata extraction + dashboard regeneration
//! - [`routes`] — HTTP routing and read-side handlers
//! - [`actions`] — mutating handlers (upload, attach, save, connect, …)
//!   and the live-mode ingest loop
//! - [`capture_registry`] — baseline/experiment slot registry
//! - [`proxy_allow`] — host-pattern allowlist for `--proxy-allow`

use super::*;

use axum::Router;
use clap::ArgMatches;
use tokio::net::TcpListener;
use tower_livereload::LiveReloadLayer;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::Ordering;

#[cfg(feature = "developer-mode")]
use notify::Watcher;

#[cfg(test)]
pub use dashboard::Kpi;
pub use dashboard::{Event, Events, ServiceExtension, TemplateRegistry};

pub use metriken_query::promql;
pub use metriken_query::tsdb;

use tsdb::*;

pub mod capture_registry;
mod proxy_allow;

mod ab_extract;
mod actions;
mod metadata;
mod report_save;
mod routes;
mod state;

use capture_registry::CaptureId;
use state::AppState;

/// Shared entry point for loading the template registry. Both the
/// viewer (`rezolus view`) and `rezolus parquet annotate/filter` call
/// this. Precedence: explicit `--templates <path>` > env var /
/// `config/templates/` default (developer-mode or explicit path only).
/// Release builds fall back to templates baked into the binary via
/// `include_dir!`; developer-mode reads from disk so template edits
/// don't require a rebuild.
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
                     (e.g. `inference-library`). Each capture's detected source \
                     (from the parquet metadata) must appear in the category \
                     template's `members` list.",
                )
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("PROXY_ALLOW")
                .long("proxy-allow")
                .value_name("HOST_PATTERN")
                .help(
                    "Enable the URL proxy and whitelist a host pattern \
                     (repeatable). Patterns are shell-style with `*` matching \
                     a single DNS label — e.g. `*.s3.amazonaws.com`, \
                     `bucket.example.internal`. Without this flag the proxy \
                     stays disabled and the browser must fetch URLs directly.",
                )
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("PROXY_ALLOW_ANY")
                .long("proxy-allow-any")
                .help(
                    "Disable the proxy allowlist entirely — every URL is \
                     fetched. Use only when you trust everyone who can reach \
                     this viewer (the SSRF risk is on you). Mutually exclusive \
                     with --proxy-allow; if both are passed, this wins.",
                )
                .action(clap::ArgAction::SetTrue),
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
    proxy_allow: proxy_allow::Allowlist,
}

/// Split a positional input into an optional alias and the remaining
/// path-or-url. The alias prefix must look "identifier-like" — no path
/// separators, no colon (would collide with URL schemes), no whitespace.
///
/// `redis=./a.parquet`         → (Some("redis"), "./a.parquet")
/// `./a.parquet`               → (None,          "./a.parquet")
/// `http://localhost:4241`     → (None,          "http://localhost:4241")
/// `/abs/path=weird.parquet`   → (None,          "/abs/path=weird.parquet")
fn split_alias(raw: &str) -> (Option<String>, &str) {
    if let Some(eq) = raw.find('=') {
        let (lhs, rest) = raw.split_at(eq);
        let rhs = &rest[1..];
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

    fn try_from(args: ArgMatches) -> Result<Self, Self::Error> {
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

        let proxy_allow = if args.get_flag("PROXY_ALLOW_ANY") {
            proxy_allow::Allowlist::any()
        } else {
            proxy_allow::Allowlist::new(
                args.get_many::<String>("PROXY_ALLOW")
                    .unwrap_or_default()
                    .cloned(),
            )
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
            proxy_allow,
        })
    }
}

pub fn run(config: Config) {
    let config: Arc<Config> = config.into();
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    ctrlc::set_handler(move || std::process::exit(2)).expect("failed to set ctrl-c handler");

    let registry = load_template_registry(config.templates_dir.as_deref());

    let mut state = match &config.source {
        Source::File(path) => init_file_mode(&config, path, &registry),
        Source::Live(url) => init_live_mode(&rt, url, &registry),
        Source::Empty => {
            info!("No input file — starting in upload-only mode");
            AppState::new(Tsdb::default(), registry.clone())
        }
    };

    state.set_proxy(config.proxy_allow.clone());
    if state.proxy.enabled() {
        if state.proxy.allow.is_any() {
            warn!(
                "URL loading enabled at /api/v1/load_url with --proxy-allow-any — \
                 every URL will be fetched. Don't expose this listen address."
            );
        } else {
            info!("URL loading enabled at /api/v1/load_url");
        }
    }

    // The experiment CLI arg is only honored when the baseline is a
    // parquet file. Live and upload-only modes use the HTTP attach
    // endpoint instead.
    if config.experiment_path.is_some() && !matches!(config.source, Source::File(_)) {
        warn!(
            "--experiment ignored outside of file mode (v1 compare requires a baseline parquet file)"
        );
    }

    let listener = std::net::TcpListener::bind(config.listen).expect("failed to listen");
    let addr = listener.local_addr().expect("socket missing local addr");

    rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if std::env::var_os("REZOLUS_NO_OPEN").is_some() {
            info!("Use your browser to view: http://{addr}");
        } else if open::that(format!("http://{addr}")).is_err() {
            info!("Use your browser to view: http://{addr}");
        } else {
            info!("Launched browser to view: http://{addr}");
        }
    });

    rt.block_on(async move { serve(listener, state).await });
    std::thread::sleep(Duration::from_millis(200));
}

/// Pull a single KV value out of a parquet file's footer. Returns
/// `None` for missing-key, malformed-file, or missing-value cases.
pub(super) fn read_footer_kv(path: &Path, key: &str) -> Option<String> {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;
    let file = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(file).ok()?;
    let kv = reader.metadata().file_metadata().key_value_metadata()?;
    kv.iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| entry.value.clone())
}

/// Build initial AppState for a parquet file source, including the
/// optional experiment attach and category validation.
fn init_file_mode(config: &Config, path: &Path, registry: &TemplateRegistry) -> AppState {
    info!("Loading data from parquet file...");

    // Combined-A/B tarball detection runs first — bare parquets fall
    // through to the normal load path below.
    if ab_extract::looks_like_ab_tarball(path) {
        return init_file_mode_combined_ab(config, path, registry);
    }

    let (systeminfo, selection, file_meta) = metadata::extract_parquet_metadata(path);
    let multinode_sysinfo = metadata::build_multinode_systeminfo(path);

    let data = Tsdb::load(path).unwrap_or_else(|e| {
        eprintln!("failed to load data from parquet: {e}");
        std::process::exit(1);
    });

    let mut service_exts = metadata::extract_service_extension_metadata(path, registry);
    metadata::validate_service_extensions(&data, &mut service_exts);

    info!("Computing file checksum...");
    let file_checksum = metadata::compute_file_checksum(path);

    if service_exts.is_empty() && config.category_name.is_some() {
        warn!("no service extension matched the baseline parquet's source metadata");
    }
    log_service_exts(&service_exts, "baseline");

    let state = AppState::new(data, registry.clone());
    *state.parquet_path.write() = Some(path.to_path_buf());
    state
        .captures
        .set_baseline_systeminfo(multinode_sysinfo.or(systeminfo));
    *state.selection.write() = selection;
    *state.file_checksum.write() = file_checksum;
    state.captures.set_baseline_file_metadata(file_meta);
    *state.trimmed_report_marker.write() =
        read_footer_kv(path, crate::parquet_metadata::KEY_REPORT);
    state
        .captures
        .set_baseline_alias(config.baseline_alias.clone());

    if let Some(ref cat_name) = config.category_name {
        validate_category_at_startup(&state, registry, cat_name, &service_exts, config);
    }

    if let Some(exp_path) = &config.experiment_path {
        attach_cli_experiment(&state, exp_path, registry, config.experiment_alias.clone());
    }

    info!("Generating dashboards...");
    metadata::regenerate_dashboards(&state);
    state
}

/// Combined-A/B file mode: extract the tar, load each per-side parquet
/// as its own Tsdb, and wire them into the baseline/experiment slots.
/// CLI baseline alias still wins over the manifest-supplied alias.
/// The CLI experiment-path flag is ignored — combined-A/B carries both
/// sides in a single artifact.
fn init_file_mode_combined_ab(
    config: &Config,
    path: &Path,
    registry: &TemplateRegistry,
) -> AppState {
    info!("Combined-A/B tarball detected — extracting per-side parquets");

    let extracted = ab_extract::extract_ab_tarball(path).unwrap_or_else(|e| {
        eprintln!("failed to extract combined-A/B tarball: {e}");
        std::process::exit(1);
    });
    let ab = extracted.manifest.clone();
    info!(
        "Loading baseline={} (sources={:?}) experiment={} (sources={:?})",
        ab.baseline.alias, ab.baseline.sources, ab.experiment.alias, ab.experiment.sources,
    );

    let (baseline_systeminfo, baseline_selection, baseline_file_meta) =
        metadata::extract_parquet_metadata(&extracted.baseline_path);
    let baseline_multinode = metadata::build_multinode_systeminfo(&extracted.baseline_path);
    let (experiment_systeminfo, _experiment_selection, experiment_file_meta) =
        metadata::extract_parquet_metadata(&extracted.experiment_path);
    let experiment_multinode = metadata::build_multinode_systeminfo(&extracted.experiment_path);

    let mut baseline_tsdb = Tsdb::load(&extracted.baseline_path).unwrap_or_else(|e| {
        eprintln!("failed to load baseline parquet from tarball: {e}");
        std::process::exit(1);
    });
    let mut experiment_tsdb = Tsdb::load(&extracted.experiment_path).unwrap_or_else(|e| {
        eprintln!("failed to load experiment parquet from tarball: {e}");
        std::process::exit(1);
    });
    // Tsdb's filename defaults to the on-disk path of the extracted parquet
    // (a tempdir path that disappears on shutdown). Replace it with the
    // tarball's filename so user-facing displays show the artifact the
    // user actually pointed at.
    let display_filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("combined-ab.parquet.ab.tar")
        .to_string();
    baseline_tsdb.set_filename(display_filename.clone());
    experiment_tsdb.set_filename(display_filename);

    let mut baseline_service_exts =
        metadata::extract_service_extension_metadata(&extracted.baseline_path, registry);
    metadata::validate_service_extensions(&baseline_tsdb, &mut baseline_service_exts);
    log_service_exts(&baseline_service_exts, "baseline");
    let mut experiment_service_exts =
        metadata::extract_service_extension_metadata(&extracted.experiment_path, registry);
    metadata::validate_service_extensions(&experiment_tsdb, &mut experiment_service_exts);
    log_service_exts(&experiment_service_exts, "experiment");

    info!("Computing file checksum...");
    let file_checksum = metadata::compute_file_checksum(path);

    let baseline_alias = config
        .baseline_alias
        .clone()
        .unwrap_or_else(|| ab.baseline.alias.clone());
    let experiment_alias = ab.experiment.alias.clone();

    let state = AppState::new(baseline_tsdb, registry.clone());
    // Point parquet_path at the *extracted* baseline parquet (not the
    // tar itself) so `regenerate_dashboards` and other parquet-aware
    // consumers can read its metadata. Same for the experiment side via
    // cli_experiment_path.
    *state.parquet_path.write() = Some(extracted.baseline_path.clone());
    *state.cli_experiment_path.write() = Some(extracted.experiment_path.clone());
    state
        .captures
        .set_baseline_systeminfo(baseline_multinode.or(baseline_systeminfo));
    *state.selection.write() = baseline_selection;
    *state.file_checksum.write() = file_checksum;
    state
        .captures
        .set_baseline_file_metadata(baseline_file_meta);
    *state.trimmed_report_marker.write() = read_footer_kv(
        &extracted.baseline_path,
        crate::parquet_metadata::KEY_REPORT,
    );
    state.captures.set_baseline_alias(Some(baseline_alias));

    state.captures.attach_experiment(
        experiment_tsdb,
        experiment_multinode.or(experiment_systeminfo),
        experiment_file_meta,
        Some(experiment_alias),
    );

    *state.combined_ab_marker.write() = Some(ab);
    // Keep the extracted tempdir alive for the duration of the process so
    // the per-side parquets remain readable for any later re-load.
    std::mem::forget(extracted);

    if config.experiment_path.is_some() {
        warn!("--experiment ignored: combined-A/B tarball already carries both sides");
    }

    // CLI --category wins. Fall back to the manifest's embedded category
    // so a tarball produced with `combine --ab --category <name>` activates
    // the bridge view without the user remembering the flag.
    let effective_category = config.category_name.clone().or_else(|| {
        state
            .combined_ab_marker
            .read()
            .as_ref()
            .and_then(|m| m.category.clone())
    });
    if let Some(ref cat_name) = effective_category {
        if config.category_name.is_none() {
            info!("Applying category {cat_name:?} from combined-A/B manifest");
        }
        validate_category_at_startup(&state, registry, cat_name, &baseline_service_exts, config);
    }

    info!("Generating dashboards...");
    metadata::regenerate_dashboards(&state);
    state
}

/// Validate `--category`: the named category must exist, both captures'
/// detected sources must appear in its `members`. Refuses to launch on
/// misconfiguration; silent fall-back hides user intent.
fn validate_category_at_startup(
    state: &AppState,
    registry: &TemplateRegistry,
    cat_name: &str,
    baseline_exts: &[(String, ServiceExtension)],
    config: &Config,
) {
    let category = registry.get_category(cat_name).unwrap_or_else(|| {
        eprintln!("no category template named {cat_name:?} found in the registry");
        std::process::exit(1);
    });
    let baseline_sources: Vec<String> = baseline_exts.iter().map(|(s, _)| s.clone()).collect();
    // Two-file mode supplies the experiment via `--experiment <path>`.
    // Combined-A/B mode unpacks both sides from a tarball and stashes the
    // extracted experiment parquet under `cli_experiment_path`. Either
    // source is fine for category validation.
    let experiment_path_owned = config
        .experiment_path
        .clone()
        .or_else(|| state.cli_experiment_path.read().clone());
    let experiment_path = experiment_path_owned.as_ref().unwrap_or_else(|| {
        eprintln!("--category {cat_name:?} requires both a baseline and an experiment capture");
        std::process::exit(1);
    });
    let mut experiment_exts =
        metadata::extract_service_extension_metadata(experiment_path, registry);
    if let Ok(exp_data) = Tsdb::load(experiment_path) {
        metadata::validate_service_extensions(&exp_data, &mut experiment_exts);
    }
    let experiment_sources: Vec<String> = experiment_exts.iter().map(|(s, _)| s.clone()).collect();
    for source in baseline_sources.iter().chain(experiment_sources.iter()) {
        if !category.members.iter().any(|m| m == source) {
            eprintln!(
                "source {source:?} is not a member of category {cat_name:?} (members: {:?})",
                category.members
            );
            std::process::exit(1);
        }
    }
    info!(
        "Activated category {:?} (members: {:?}) — baseline sources {:?}, experiment sources {:?}",
        cat_name, category.members, baseline_sources, experiment_sources,
    );
    *state.category_name.write() = Some(cat_name.to_string());
}

/// Attach the optional experiment capture supplied via the CLI. Records
/// the user-supplied path in `cli_experiment_path` (NOT
/// `experiment_parquet_path`) so detach never deletes the user's file.
fn attach_cli_experiment(
    state: &AppState,
    exp_path: &Path,
    registry: &TemplateRegistry,
    alias: Option<String>,
) {
    info!("Loading experiment from parquet file...");
    let (exp_sysinfo, _exp_selection, exp_file_meta) = metadata::extract_parquet_metadata(exp_path);
    let mut exp_tsdb = match Tsdb::load(exp_path) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                "failed to load experiment '{}': {e}. Starting in single-capture mode.",
                exp_path.display(),
            );
            return;
        }
    };
    let base = exp_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("experiment.parquet")
        .to_string();
    exp_tsdb.set_filename(base);
    state
        .captures
        .attach_experiment(exp_tsdb, exp_sysinfo, exp_file_meta, alias);
    *state.cli_experiment_path.write() = Some(exp_path.to_path_buf());
    info!("Attached experiment capture: {}", exp_path.display());

    let mut exp_exts = metadata::extract_service_extension_metadata(exp_path, registry);
    if let Some(handle) = state.captures.get(CaptureId::Experiment) {
        let exp_data = handle.read();
        metadata::validate_service_extensions(&exp_data, &mut exp_exts);
    }
    if exp_exts.is_empty() {
        warn!("no service extension matched the experiment parquet's source metadata");
    }
    log_service_exts(&exp_exts, "experiment");
}

fn log_service_exts(exts: &[(String, ServiceExtension)], capture: &str) {
    for (source, ext) in exts {
        let available = ext.kpis.iter().filter(|k| k.available).count();
        info!(
            "Found service extension for {:?} from source {:?} ({}/{} KPIs available) — {capture}",
            ext.service_name,
            source,
            available,
            ext.kpis.len()
        );
    }
}

fn init_live_mode(
    rt: &tokio::runtime::Runtime,
    url: &Url,
    registry: &TemplateRegistry,
) -> AppState {
    info!("Connecting to live agent at {url}...");
    let info = rt.block_on(async {
        let client = Client::builder()
            .http1_only()
            .build()
            .expect("failed to create http client");
        match actions::fetch_agent_info(&client, url).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    });
    info!(
        "Connected to {source} {version} at {url}",
        source = info.source,
        version = info.version
    );

    let mut tsdb = Tsdb::default();
    tsdb.set_sampling_interval_ms(1000);
    tsdb.set_source(info.source.clone());
    tsdb.set_version(info.version.clone());
    tsdb.set_filename(url.to_string());
    let state = AppState::new(tsdb, registry.clone());
    let context = dashboard::dashboard::build_dashboard_context(None, &[], None);
    *state.sections.write() = state::LazySectionStore::new(context);
    state.live.store(true, Ordering::Relaxed);
    state.captures.set_baseline_systeminfo(info.sysinfo);

    let ingest_tsdb = state.baseline_tsdb();
    let ingest_snapshots = state.snapshots.clone();
    let mut ingest_url = url.clone();
    ingest_url.set_path("/metrics/binary");

    rt.spawn(actions::ingest_loop(
        ingest_url,
        ingest_tsdb,
        ingest_snapshots,
        info.source,
        info.version,
    ));

    state
}

async fn serve(listener: std::net::TcpListener, state: AppState) {
    let livereload = LiveReloadLayer::new();

    #[cfg(feature = "developer-mode")]
    {
        let reloader = livereload.reloader();
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(event) = res {
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

    let app: Router = routes::app(livereload, state);

    listener.set_nonblocking(true).unwrap();
    let listener = TcpListener::from_std(listener).unwrap();
    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

#[cfg(test)]
mod tests {
    use super::routes::strip_sections_from_section_payload;
    use super::state::{build_sections_metadata_payload, LazySectionStore};
    use ::dashboard::dashboard::DashboardContext;
    use ::dashboard::Section;

    #[test]
    fn lean_section_payload_does_not_repeat_sections() {
        let mut payload = serde_json::json!({
            "sections": [{"name": "Overview", "route": "/overview"}],
            "groups": [],
            "interval": 1.0
        });
        strip_sections_from_section_payload(&mut payload);
        assert!(payload.get("sections").is_none());
        assert_eq!(payload["groups"], serde_json::json!([]));
    }

    #[test]
    fn sections_metadata_json_omits_groups() {
        let sections = vec![
            serde_json::json!({"name": "Overview", "route": "/overview"}),
            serde_json::json!({"name": "CPU", "route": "/cpu"}),
        ];
        let payload = build_sections_metadata_payload(
            sections.clone(),
            "rezolus",
            "test-version",
            "capture.parquet",
            1.0,
            42,
            1000,
            2000,
            99,
        );
        assert_eq!(payload["sections"], serde_json::Value::Array(sections));
        assert!(payload.get("groups").is_none());
        assert_eq!(payload["source"], serde_json::json!("rezolus"));
        assert_eq!(payload["version"], serde_json::json!("test-version"));
        assert_eq!(payload["filename"], serde_json::json!("capture.parquet"));
        assert_eq!(payload["interval"], serde_json::json!(1.0));
        assert_eq!(payload["filesize"], serde_json::json!(42u64));
        assert_eq!(payload["start_time"], serde_json::json!(1000u64));
        assert_eq!(payload["end_time"], serde_json::json!(2000u64));
        assert_eq!(payload["num_series"], serde_json::json!(99usize));
    }

    #[test]
    fn lazy_section_store_exposes_context_sections() {
        let context = DashboardContext {
            sections: vec![
                Section {
                    name: "Overview".to_string(),
                    route: "/overview".to_string(),
                },
                Section {
                    name: "CPU".to_string(),
                    route: "/cpu".to_string(),
                },
            ],
            ..DashboardContext::default()
        };
        let store = LazySectionStore::new(context);
        assert_eq!(store.sections().len(), 2);
        assert!(!store.is_empty());
    }

    #[test]
    fn lazy_section_store_default_is_empty() {
        let store = LazySectionStore::default();
        assert!(store.is_empty());
        assert_eq!(store.sections().len(), 0);
    }
}

#[cfg(test)]
mod report_kv_tests {
    use super::read_footer_kv;
    use arrow::array::UInt64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    #[test]
    fn reads_present_key() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "timestamp",
            DataType::UInt64,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(UInt64Array::from(vec![1u64]))],
        )
        .unwrap();
        let kv = vec![KeyValue {
            key: "report".to_string(),
            value: Some("trimmed".to_string()),
        }];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut writer = ArrowWriter::try_new(tmp.reopen().unwrap(), schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        assert_eq!(
            read_footer_kv(tmp.path(), "report"),
            Some("trimmed".to_string())
        );
        assert_eq!(read_footer_kv(tmp.path(), "missing"), None);
    }
}
