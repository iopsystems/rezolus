//! Viewer subcommand: serves a web dashboard backed by .dendro archives,
//! a live agent connection, or upload-only mode.
//!
//! Submodules:
//! - [`state`] — `AppState`, `LazySectionStore`, `CaptureParam`, API
//!   envelope types
//! - [`metadata`] — capture metadata + dashboard regeneration
//! - [`routes`] — HTTP routing and read-side handlers
//! - [`actions`] — mutating handlers (upload, attach, save, connect, …)
//!   and the live-mode ingest loop
//! - [`follow`] — reopening an opened archive file while its writer appends
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
use std::sync::Arc;

#[cfg(feature = "developer-mode")]
use notify::Watcher;

pub use dashboard::{Event, Events, ServiceExtension, TemplateRegistry};

/// Re-export PromQL types so other modules can use `crate::viewer::promql::*`.
pub mod promql {}

pub mod capture_registry;
mod proxy_allow;

mod actions;
#[cfg(test)]
pub(crate) mod combined_test;
#[cfg(test)]
mod dashboard_queries_test;
mod follow;
mod live;
pub(crate) mod metadata;
mod report_save;
mod routes;
mod state;
mod tui;

use ::dashboard::source_templates::TemplatePreference;
use capture_registry::CaptureId;
use state::AppState;

/// Shared entry point for loading the template registry. Both the
/// viewer (`rezolus view`) and `rezolus recording annotate/check` call
/// this. Precedence: explicit `--templates <path>` > env var /
/// `crates/dashboard/templates/` default (developer-mode or explicit path only).
/// Release builds fall back to the templates baked into the `dashboard`
/// crate via `include_dir!`, so the binary and any other consumer of that
/// crate render the same set; developer-mode reads from disk so template
/// edits don't require a rebuild.
pub fn load_template_registry(cli_path: Option<&Path>) -> TemplateRegistry {
    if cli_path.is_some() {
        return ::dashboard::resolve_templates(cli_path);
    }
    #[cfg(not(feature = "developer-mode"))]
    {
        ::dashboard::embedded_templates().unwrap_or_else(|e| {
            warn!("failed to parse embedded templates: {e}");
            TemplateRegistry::empty()
        })
    }
    #[cfg(feature = "developer-mode")]
    {
        ::dashboard::resolve_templates(None)
    }
}

/// The input source for the viewer.
enum Source {
    /// A recording on disk.
    File(PathBuf),
    /// A live Rezolus agent URL.
    Live(Url),
    /// No input — upload-only mode.
    Empty,
}

pub fn command() -> Command {
    Command::new("view")
        .about("Serve an interactive web dashboard for a recording or live agent")
        .long_about(
            "Launch a local web dashboard to explore Rezolus metrics. PromQL runs in the\n\
             browser (WASM), so data stays on your machine. A browser is opened automatically\n\
             unless REZOLUS_NO_OPEN is set.\n\n\
             WHAT TO VIEW (pick one input form):\n    \
             - a recording:              rezolus view rezolus.dendro\n    \
             - two recordings (A/B):     rezolus view baseline.dendro experiment.dendro\n    \
             - a live agent:             rezolus view http://host:4241   (agent 5.21.0+ with snapshot_format = \"v3\"; recorded into a temporary .dendro)\n    \
             - nothing (upload-only):    rezolus view    (drag files in from the browser)\n\n\
             A recording is a .dendro archive. A .rez archive, a .parquet file or a\n\
             .parquet.ab.tar from an earlier release is refused with the command that\n\
             converts it: `rezolus recording upgrade FILE`.\n\n\
             An archive holding several recordings of one run (`rezolus record --endpoint a\n\
             --endpoint b`) opens as ONE combined view: every series carries its recording's\n\
             labels (source, host, record's labels), and each recording renders its own sections, so\n\
             two agents show the built-in sections once per host (the second host's routes\n\
             end in @<host>, e.g. #/cpu@web-02) and a service shows the dashboard its\n\
             recording's template describes. A/B is opt-in: the archive opens as a\n\
             baseline/experiment comparison only when --baseline/--experiment name its\n\
             recordings by label (repeatable, ANDed, subset match; each must name exactly one\n\
             recording) or its recordings carry an `arm` label (`rezolus record --endpoint\n\
             url,arm=baseline`, or `rezolus recording combine a b --ab baseline=a\n\
             experiment=b`). Without selectors the arms fill the slots, whatever the\n\
             archive's order: arm=baseline is the baseline and arm=experiment the experiment\n\
             (ignoring case); with one of them, the other arm fills the other slot; with\n\
             neither, the archive's first armed recording is the baseline. Every other\n\
             recording (one without an arm, such as an agent recorded beside two arms of a\n\
             service, or one --baseline/--experiment did not name) is shown beside the\n\
             comparison in its own sections, at routes ending in @<key> (its host, or the\n\
             labels that tell it apart), as a single capture; when the baseline has no\n\
             system metrics the page opens on the comparison's first service section. One\n\
             recording per arm: one arm among unarmed recordings, an arm two recordings\n\
             share, or more than two arms is refused with a listing; name two with\n\
             --baseline/--experiment.\n\
             `rezolus mcp describe-recording FILE` lists an archive's recordings.\n\n\
             Either positional may be given as alias=path (e.g. redis=./a.dendro) to set the\n\
             display label; the internal slots stay baseline/experiment. The second positional\n\
             is the experiment of a two-file comparison: each file is then one recording (an\n\
             archive of several is refused, naming the `recording combine` and\n\
             --baseline/--experiment commands that compare two of its recordings). Live and\n\
             upload-only modes attach a second capture from the browser instead.\n\n\
             By default the viewer binds 127.0.0.1 on a random free port; pass --listen to fix\n\
             the address (e.g. to reach it from another host).\n\n\
             The --templates/--category flags (service-extension dashboards) and the --proxy-*\n\
             flags (in-browser 'load from URL' fetching) are advanced; the input forms above\n\
             need none of them. A recording that carries a source's KPI template in its\n\
             metadata renders it ahead of the built-in one, and a stream recording of a\n\
             Rezolus agent (6.0 or later) renders the built-in sections (CPU, network, ...)\n\
             from the section template the agent sent; --registry-templates renders the\n\
             viewer's own (or --templates DIR) templates instead, for both.\n\n\
             TERMINAL UI:\n    \
             Pass --tui to render in the terminal instead of a web browser: a live overview\n    \
             plus a drill-down browser of the same dashboard sections (CPU, network,\n    \
             scheduler, ...). It needs a recording or a live agent URL as input; upload-only\n    \
             (no input) is not supported and errors. It shows a SINGLE capture — A/B compare\n    \
             and the --category bridge view are browser-only, so a second positional file or\n    \
             --category is ignored (with a warning). No browser window is opened, no HTTP is\n    \
             served, and --listen/--proxy-* are ignored. Keys: Tab/o overview<->browser, j/k\n    \
             move, Enter/l open, Esc/h back, [ / ] time window, r refresh, ? help, q quit.\n\n\
             EXAMPLES:\n    \
             # Open a single recording\n    \
             rezolus view rezolus.dendro\n\n    \
             # Explore a recording in the terminal (no browser)\n    \
             rezolus view --tui rezolus.dendro\n\n    \
             # Stream a live agent in the terminal UI\n    \
             rezolus view --tui http://localhost:4241\n\n    \
             # A/B compare two recordings with display labels\n    \
             rezolus view redis=baseline.dendro valkey=experiment.dendro\n\n    \
             # One run of two hosts: one view, each host's sections\n    \
             rezolus view fleet.dendro\n\n    \
             # The same archive as an A/B comparison of the two hosts\n    \
             rezolus view fleet.dendro --baseline host=web-01 --experiment host=web-02\n\n    \
             # Stream live from a remote agent on a fixed address\n    \
             rezolus view http://host:4241 --listen 127.0.0.1:8080\n\n    \
             # Upload-only mode: start with no file, then upload from the browser\n    \
             rezolus view\n\n    \
             # A/B compare using a category bridge view (both sources must be category members)\n    \
             rezolus view a.dendro b.dendro --category inference-library",
        )
        .arg(
            clap::Arg::new("INPUT")
                .help(
                    "First capture: a .dendro recording, an agent URL (http://…), \
                     or alias=path (e.g. redis=./a.dendro). The alias is a \
                     display label; internal identifiers stay baseline/experiment. \
                     An archive of several recordings of one run opens as one combined \
                     view; see --baseline for a comparison.",
                )
                .action(clap::ArgAction::Set)
                .required(false)
                .index(1),
        )
        .arg(
            clap::Arg::new("EXPERIMENT")
                .help(
                    "Optional second capture for A/B comparison: a one-recording .dendro \
                     or alias=path (e.g. valkey=./b.dendro).",
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
            clap::Arg::new("REGISTRY_TEMPLATES")
                .long("registry-templates")
                .help(
                    "Render from the viewer's own templates instead of the ones the \
                     recording carries in its metadata. KPI sections come from the \
                     template registry (the built-in templates, or --templates DIR) \
                     rather than `service_queries` (written by `rezolus recording \
                     annotate` or by the producer); a source the registry does not \
                     know keeps its own template either way. The built-in sections \
                     (Overview, CPU, ...) come from the section template this viewer \
                     embeds rather than `dashboard` (sent by a Rezolus agent in its \
                     stream handshake). By default the recording's templates win.",
                )
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("CATEGORY")
                .long("category")
                .value_name("NAME")
                .help(
                    "Activate the A/B 'category bridge' view using the named category \
                     template (e.g. `inference-library`). NAME must match a category \
                     template known to the viewer (built-in, or from --templates). Each \
                     capture's detected source (from its metadata) must appear in \
                     that template's `members` list. Only meaningful when comparing two \
                     captures.",
                )
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("PROXY_ALLOW")
                .long("proxy-allow")
                .value_name("HOST_PATTERN")
                .help(
                    "Let the browser's 'load from URL' feature fetch recording/agent \
                     URLs through this server, and whitelist a host pattern \
                     (repeatable). Patterns are shell-style with `*` matching \
                     a single DNS label — e.g. `--proxy-allow '*.s3.amazonaws.com'`, \
                     `--proxy-allow bucket.example.internal`. Without this flag the \
                     proxy stays disabled and the browser fetches URLs directly. Not \
                     needed for the file/live/upload input forms.",
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
        .arg(
            clap::Arg::new("TUI")
                .long("tui")
                .help(
                    "Show a terminal UI instead of a web browser dashboard. Needs an input: \
                     a recording or a live agent URL. Upload-only mode (no input) is \
                     browser-only, so `rezolus view --tui` with no input is an error. Shows a \
                     single capture: A/B compare and the --category bridge view are \
                     browser-only, so a second positional file or --category is ignored (with \
                     a warning). No browser window is opened and no HTTP is served, so \
                     --listen and the --proxy-* flags are ignored. Keys: Tab/o \
                     overview<->browser, j/k move, Enter/l open, Esc/h back, [ / ] time \
                     window, r refresh, ? help, q quit.",
                )
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("BASELINE_RECORDING")
                .long("baseline")
                .value_name("KEY=VALUE")
                .help(
                    "Open a multi-recording archive as an A/B comparison, with this \
                     recording in the baseline slot, named by labels (repeatable, ANDed; a \
                     recording matches when it carries every pair given). Must name exactly \
                     one recording: matching none or several is an error listing the \
                     candidates. On its own it shows that one recording; pair it with \
                     --experiment to choose both sides of the A/B. Without either flag an \
                     archive of one run opens as one combined view. `rezolus mcp \
                     describe-recording FILE` lists an archive's recordings and their labels.",
                )
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("EXPERIMENT_RECORDING")
                .long("experiment")
                .value_name("KEY=VALUE")
                .help(
                    "Which recording of a multi-recording archive fills the experiment slot \
                     (repeatable, ANDed). Needs --baseline: it names the second arm of a \
                     comparison, not the first.",
                )
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("CACHE_SIZE_MB")
                .long("cache-size-mb")
                .value_name("MB")
                .help(
                    "Row-group buffer cache size in megabytes. Larger values \
                     speed up repeated queries against the same data (typical \
                     dashboard workload); smaller values reduce peak RSS. \
                     Overrides REZOLUS_CACHE_MB. Default: an eighth of the \
                     memory available (physical, or a Linux cgroup's limit \
                     when lower), clamped to 500-8192.",
                )
                .value_parser(value_parser!(usize))
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("QUERY_CONCURRENCY")
                .long("query-concurrency")
                .value_name("N")
                .help(
                    "How many requests may read a recording at once: queries, \
                     dashboard sections, saving a report, and loading an upload, \
                     a URL or an experiment or detaching one. The rest wait for \
                     a free slot. \
                     Overrides REZOLUS_QUERY_CONCURRENCY. Default: half the \
                     CPUs, from 2 to 8.",
                )
                .value_parser(value_parser!(u64).range(1..))
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
    /// Whose KPI template renders when a recording carries one and the
    /// registry knows the source too (`--registry-templates`).
    template_preference: TemplatePreference,
    proxy_allow: proxy_allow::Allowlist,
    /// Buffer pool budget in bytes. Defaults to `default_cache_size_bytes()`.
    cache_size_bytes: usize,
    /// Queries run at once. Defaults to `state::default_query_concurrency`.
    query_concurrency: usize,
    /// Render a terminal UI instead of the web dashboard.
    tui: bool,
    /// Label selectors naming which recordings of a multi-recording archive
    /// fill the A/B slots. An empty selector means the flag was not given.
    baseline_recording: crate::mcp::RecordingSelector,
    experiment_recording: crate::mcp::RecordingSelector,
}

impl Config {
    /// Whether either recording selector was given.
    fn selects_a_recording(&self) -> bool {
        !self.baseline_recording.is_empty() || !self.experiment_recording.is_empty()
    }
}

/// Split a positional input into an optional alias and the remaining
/// path-or-url. The alias prefix must look "identifier-like" — no path
/// separators, no colon (would collide with URL schemes), no whitespace.
///
/// `redis=./a.dendro`          → (Some("redis"), "./a.dendro")
/// `./a.dendro`                → (None,          "./a.dendro")
/// `http://localhost:4241`     → (None,          "http://localhost:4241")
/// `/abs/path=weird.dendro`    → (None,          "/abs/path=weird.dendro")
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

        // Resolve cache size: CLI flag > env var > default_cache_size_bytes().
        let cache_size_bytes = args
            .get_one::<usize>("CACHE_SIZE_MB")
            .copied()
            .or_else(|| {
                std::env::var("REZOLUS_CACHE_MB")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or_else(state::default_cache_size_bytes);

        // Queries at once: CLI flag > env var > half the CPUs.
        let query_concurrency = args
            .get_one::<u64>("QUERY_CONCURRENCY")
            .map(|n| *n as usize)
            .or_else(|| {
                std::env::var("REZOLUS_QUERY_CONCURRENCY")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
            })
            .unwrap_or_else(state::default_query_concurrency)
            .max(1);

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
            template_preference: if args.get_flag("REGISTRY_TEMPLATES") {
                TemplatePreference::Registry
            } else {
                TemplatePreference::Archive
            },
            proxy_allow,
            cache_size_bytes,
            query_concurrency,
            tui: args.get_flag("TUI"),
            baseline_recording: crate::mcp::RecordingSelector::parse(
                BASELINE_FLAG,
                args.get_many::<String>("BASELINE_RECORDING")
                    .unwrap_or_default()
                    .cloned(),
            )?,
            experiment_recording: crate::mcp::RecordingSelector::parse(
                EXPERIMENT_FLAG,
                args.get_many::<String>("EXPERIMENT_RECORDING")
                    .unwrap_or_default()
                    .cloned(),
            )?,
        })
    }
}

pub fn run(config: Config) {
    let config: Arc<Config> = config.into();
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // The TUI installs its own SIGINT handler (one that restores the
    // terminal first); a bare process::exit here would bypass that and
    // leave the terminal in raw mode. In raw mode a keyboard Ctrl-C is
    // delivered as a normal key event and handled as a graceful quit.
    if !config.tui {
        ctrlc::set_handler(move || {
            // `exit` runs no destructors, so the live archive is removed here.
            live::remove_live_dirs();
            std::process::exit(2)
        })
        .expect("failed to set ctrl-c handler");
    }

    let registry = load_template_registry(config.templates_dir.as_deref());

    // One shared pool for all ParquetReaders in this process.
    let pool = metriken_storage::BufferPool::new(config.cache_size_bytes);
    info!(
        "Buffer pool: {} MB",
        config.cache_size_bytes / (1024 * 1024)
    );

    let mut state = match &config.source {
        Source::File(path) => init_file_mode(&config, path, &registry, Arc::clone(&pool)),
        Source::Live(url) => {
            if config.selects_a_recording() {
                eprintln!(
                    "{}",
                    selectors_need_an_archive(&format!("the live agent at {url}"))
                );
                std::process::exit(1);
            }
            init_live_mode(
                &rt,
                url,
                &registry,
                config.template_preference,
                Arc::clone(&pool),
            )
        }
        Source::Empty => {
            if config.selects_a_recording() {
                eprintln!(
                    "{}",
                    selectors_need_an_archive("upload-only mode (no input file)")
                );
                std::process::exit(1);
            }
            info!("No input file — starting in upload-only mode");
            let empty_store: std::sync::Arc<dyn metriken_query::MetricsSource> =
                std::sync::Arc::new(metriken_query::MemoryStore::builder().build());
            AppState::with_pool(
                empty_store,
                registry.clone(),
                config.template_preference,
                Arc::clone(&pool),
            )
        }
    };

    state.set_proxy(config.proxy_allow.clone());
    state.set_query_concurrency(config.query_concurrency);
    if !config.tui {
        info!("Queries at once: {}", config.query_concurrency);
    }
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
    // file. Live and upload-only modes use the HTTP attach
    // endpoint instead.
    if config.experiment_path.is_some() && !matches!(config.source, Source::File(_)) {
        warn!(
            "the second positional is ignored outside of file mode (a two-file comparison needs a baseline file)"
        );
    }

    if config.tui {
        if matches!(config.source, Source::Empty) {
            eprintln!("--tui needs a data source: a live agent URL or a recording (upload-only is not supported)");
            std::process::exit(1);
        }
        // The TUI shows a single capture. A/B compare and the category
        // bridge view are browser-only, so a second capture / --category
        // is ignored rather than silently changing what's displayed.
        if config.experiment_path.is_some() {
            warn!("second capture ignored in --tui mode (A/B compare is browser-only); showing the first input only");
        }
        if config.category_name.is_some() {
            warn!("--category ignored in --tui mode (the category bridge view is browser-only)");
        }
        let live = matches!(config.source, Source::Live(_));
        tui::run_tui(state, live, &rt);
        // Dropping the runtime stops the live session's subscription, which
        // ends its recording thread; wait for the thread to close its writer,
        // then remove the archive directory, since the process would exit
        // before the thread let go of it.
        drop(rt);
        live::finish_live_sessions(Duration::from_secs(5));
        return;
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

/// Build the initial AppState for a recording file: a dendro archive, with
/// an optional second one-recording archive as the experiment of a
/// comparison. Any other file is refused, one an earlier release wrote with
/// the `recording upgrade` command that converts it.
fn init_file_mode(
    config: &Config,
    path: &Path,
    registry: &TemplateRegistry,
    pool: Arc<metriken_storage::BufferPool>,
) -> AppState {
    for file in std::iter::once(path).chain(config.experiment_path.as_deref()) {
        if let Err(e) = ::rez::container::require_dendro(file, env!("CARGO_BIN_NAME")) {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
    init_file_mode_rez(config, path, registry, pool)
}

/// Whether the baseline's recording carries an `arm` label: then the
/// baseline file's recordings cannot be relabelled by `combine --ab`.
pub(super) fn baseline_has_arms(state: &AppState) -> bool {
    state
        .anchor_labels
        .read()
        .as_ref()
        .is_some_and(|l| l.contains_key(::dashboard::combined::ARM_LABEL))
}

/// Open the dendro archive at `path` as the experiment of a two-file
/// comparison: its one recording, with the systeminfo and metadata the
/// captures registry keeps. A legacy file is refused with the upgrade
/// command, an archive of several recordings with the commands that compare
/// two of them ([`::rez::container::one_recording_hint`]), which name the
/// experiment as `name` and the baseline as `baseline`; `baseline_arms` is
/// whether the baseline's recordings carry `arm` labels.
pub(super) fn open_experiment(
    path: &Path,
    name: &str,
    baseline: &str,
    baseline_arms: bool,
    pool: Arc<metriken_storage::BufferPool>,
) -> Result<(crate::rez_reader::RezReader, Option<String>, Option<String>), String> {
    use metriken_query::MetricsSource;
    ::rez::container::require_dendro(path, env!("CARGO_BIN_NAME"))?;
    let mut readers = crate::rez_reader::RezReader::open_recordings(path, pool)
        .map_err(|e| format!("failed to load {}: {e}", path.display()))?;
    if readers.len() != 1 {
        return Err(::rez::container::one_recording_hint(
            env!("CARGO_BIN_NAME"),
            name,
            "the experiment",
            readers.len(),
            baseline,
            name,
            readers
                .iter()
                .any(|(l, _)| l.contains_key(::dashboard::combined::ARM_LABEL))
                || baseline_arms,
        ));
    }
    let (_, reader) = readers.pop().expect("one recording");
    let systeminfo = reader.metadata_get("systeminfo");
    let file_meta = file_meta_json(&reader);
    Ok((reader, systeminfo, file_meta))
}

/// Which label tells a set of recordings apart. Shared with the WASM viewer so
/// an archive is named the same wherever it is opened.
pub(crate) use ::dashboard::capture_alias::discriminating_alias_key;

/// The flag naming the recording that fills the viewer's baseline slot.
const BASELINE_FLAG: &str = "--baseline";
/// The flag naming the recording that fills the viewer's experiment slot.
const EXPERIMENT_FLAG: &str = "--experiment";
const BASELINE_SYNTAX: crate::mcp::SelectorSyntax = crate::mcp::SelectorSyntax::Flag(BASELINE_FLAG);
const EXPERIMENT_SYNTAX: crate::mcp::SelectorSyntax =
    crate::mcp::SelectorSyntax::Flag(EXPERIMENT_FLAG);

/// Resolve one slot's selector to a single recording, or say why it did not.
///
/// `flag`/`syntax` are the slot's own flag: a caller told to add labels has
/// to be told to add them to the flag it typed.
fn resolve_slot(
    all: &[std::collections::BTreeMap<String, String>],
    selector: &crate::mcp::RecordingSelector,
    syntax: crate::mcp::SelectorSyntax,
) -> Result<usize, String> {
    selector.resolve(all).map_err(|e| match e {
        crate::mcp::SelectError::NoMatch => format!(
            "{} matched no recording in this archive.\nArchive holds {} recording(s):\n{}",
            selector.render(syntax),
            all.len(),
            crate::mcp::describe_candidates(all, &[], syntax, env!("CARGO_BIN_NAME")),
        ),
        crate::mcp::SelectError::Ambiguous(hits) => format!(
            "{} matched {} recordings; add labels until it names one:\n{}",
            selector.render(syntax),
            hits.len(),
            crate::mcp::describe_candidates(all, &hits, syntax, env!("CARGO_BIN_NAME")),
        ),
    })
}

/// Which recordings of a multi-recording archive fill the baseline and
/// experiment slots.
///
/// With neither selector the recordings' `arm` labels decide
/// (`dashboard::combined::arm_slots`); with `--baseline` the selectors do.
///
/// A selector that matches nothing, or several, is refused rather than
/// narrowed to a first match — the same call made for the MCP `--recording`
/// flag, and for the same reason: rendering one arm of a comparison under
/// another arm's name is a clean-looking wrong answer.
fn resolve_ab_slots(
    all: &[std::collections::BTreeMap<String, String>],
    baseline: &crate::mcp::RecordingSelector,
    experiment: &crate::mcp::RecordingSelector,
) -> Result<(usize, Option<usize>), String> {
    if baseline.is_empty() && experiment.is_empty() {
        // The arms say which is which (`dashboard::combined::arm_slots`, as
        // the WASM viewer reads them).
        return match ::dashboard::combined::arm_slots(all) {
            Ok(slots) => {
                if let Some(note) = &slots.note {
                    info!("{note}");
                }
                Ok((slots.baseline, slots.experiment))
            }
            Err(why) => Err(format!(
                "{why}. Name the two with {BASELINE_FLAG}/{EXPERIMENT_FLAG}.\nArchive holds {} \
                 recording(s):\n{}",
                all.len(),
                crate::mcp::describe_candidates(all, &[], BASELINE_SYNTAX, env!("CARGO_BIN_NAME")),
            )),
        };
    }

    // `--experiment` alone would leave the baseline slot to fall back on
    // recording 0 — which may well BE the recording just named, and a
    // comparison of a recording against itself is not a comparison. Refusing
    // is also the honest reading: the flags name the two sides of one
    // comparison, so naming only the second is an incomplete instruction, not
    // an instruction with a default.
    if baseline.is_empty() {
        return Err(format!(
            "{} names the second arm of a comparison, so {BASELINE_FLAG} has to name the \
             first.\nArchive holds {} recording(s):\n{}",
            experiment.render(EXPERIMENT_SYNTAX),
            all.len(),
            crate::mcp::describe_candidates(all, &[], BASELINE_SYNTAX, env!("CARGO_BIN_NAME")),
        ));
    }

    let b = resolve_slot(all, baseline, BASELINE_SYNTAX)?;
    if experiment.is_empty() {
        // A baseline selector on its own is "show me this one arm", not "show
        // me this arm against whatever is next to it": pairing it with an
        // unnamed recording would invent the other half of a comparison the
        // caller did not ask for.
        return Ok((b, None));
    }
    let e = resolve_slot(all, experiment, EXPERIMENT_SYNTAX)?;
    if b == e {
        return Err(format!(
            "{BASELINE_FLAG} and {EXPERIMENT_FLAG} both name the same recording ({}); a \
             comparison needs two. Both are subset matches, so a selector naming fewer \
             labels can land on the recording another one already named.",
            crate::mcp::render_labels(&all[b]),
        ));
    }
    Ok((b, Some(e)))
}

/// The message for a recording selector passed where there is no archive to
/// select inside of. `what` names the input as the user gave it.
fn selectors_need_an_archive(what: &str) -> String {
    format!(
        "{BASELINE_FLAG}/{EXPERIMENT_FLAG} name recordings inside a multi-recording .dendro \
         archive, but {what} is not one.\nTo compare two separate files, pass them as \
         positionals: {} view baseline.dendro experiment.dendro",
        env!("CARGO_BIN_NAME")
    )
}

/// A capture's metadata as the JSON object `/api/v1/file_metadata` serves.
pub(super) fn file_meta_json(reader: &dyn metriken_query::MetricsSource) -> Option<String> {
    let mut map = serde_json::Map::new();
    for (k, v) in reader.file_metadata() {
        let jv = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v.clone()));
        map.insert(k, jv);
    }
    serde_json::to_string(&serde_json::Value::Object(map)).ok()
}

/// Whether an archive's recordings open as one combined view: more than one
/// recording, and `dashboard::combined::layout` says combined.
/// `compare_requested` is `--baseline`/`--experiment`.
pub(crate) fn opens_combined(
    readers: &crate::rez_reader::LabeledRecordings,
    compare_requested: bool,
) -> bool {
    let labels: Vec<std::collections::BTreeMap<String, String>> =
        readers.iter().map(|(l, _)| l.clone()).collect();
    readers.len() > 1
        && ::dashboard::combined::layout(&labels, compare_requested)
            == ::dashboard::combined::Layout::Combined
}

/// An archive's recordings joined into one source (`RezReader::combine`),
/// and what the viewer keeps about each.
pub(super) struct CombinedRecordings {
    pub reader: crate::rez_reader::RezReader,
    /// Each recording, for `build_combined_context`.
    pub members: Vec<::dashboard::combined::Member>,
    /// Every recording's labels, in the archive's order.
    pub labels: Vec<std::collections::BTreeMap<String, String>>,
    /// The recordings' systeminfo (`dashboard::combined::assemble_members`).
    pub systeminfo: Option<String>,
    /// A report's selection, marker and events, from whichever recording
    /// carries the selection (`dashboard::combined::report_markers`): a
    /// report saved from a comparison of this archive sits on its baseline,
    /// which need not be the first recording the joined reader reads.
    pub markers: Vec<(&'static str, String)>,
}

/// Join `readers` into the combined source. A recording without rows is
/// warned about, as file mode does for any recording.
pub(super) fn combine_recordings(
    readers: crate::rez_reader::LabeledRecordings,
) -> CombinedRecordings {
    use metriken_query::MetricsSource;
    let labels: Vec<_> = readers.iter().map(|(l, _)| l.clone()).collect();
    let mut parts = Vec::with_capacity(readers.len());
    for (l, reader) in &readers {
        if reader.is_empty() {
            warn!(
                "recording {} holds no rows yet",
                crate::mcp::render_labels(l)
            );
        }
        parts.push((
            l.clone(),
            ::dashboard::combined::Header::of(reader),
            reader.file_metadata(),
        ));
    }
    let markers = ::dashboard::combined::report_markers(
        &readers
            .iter()
            .map(|(_, r)| r as &dyn MetricsSource)
            .collect::<Vec<_>>(),
    );
    let filename = readers.first().and_then(|(_, r)| r.filename());
    let (reader, series) = crate::rez_reader::RezReader::combine(readers, filename);
    let (members, systeminfo) = ::dashboard::combined::assemble_members(
        parts
            .into_iter()
            .zip(series)
            .map(|((l, header, metadata), series)| (l, series, header, metadata))
            .collect(),
    );
    CombinedRecordings {
        reader,
        members,
        labels,
        systeminfo,
        markers,
    }
}

/// A followed combined archive's [`::dashboard::combined::ScopeCatalog`]:
/// the archive as the live reader reads it now.
pub(super) struct LiveCatalog(pub Arc<rez::live::LiveReader>);

impl ::dashboard::combined::ScopeCatalog for LiveCatalog {
    fn archive(
        &self,
    ) -> Arc<dyn std::ops::Deref<Target = metriken_storage::ArchiveReader> + Send + Sync> {
        self.0.current()
    }
}

/// File mode for an archive that opens combined: one capture, the
/// recordings joined, followed as one when the archive is not finalized.
fn init_file_mode_combined(
    config: &Config,
    path: &Path,
    registry: &TemplateRegistry,
    pool: Arc<metriken_storage::BufferPool>,
    readers: crate::rez_reader::LabeledRecordings,
) -> AppState {
    let combined = combine_recordings(readers);
    info!(
        "{} holds {} recordings of one run; opening them as one view (--baseline/--experiment \
         open a comparison)",
        path.display(),
        combined.labels.len()
    );
    let markers = combined.markers.clone();
    let file_meta = file_meta_json(&combined.reader)
        .map(|meta| ::dashboard::combined::with_report_markers(&meta, &markers));
    let selection = ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_SELECTION);
    let report_marker =
        ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_REPORT);
    // An archive not finalized when opened is followed: it can grow in place.
    let following = !combined.reader.complete();
    let mut live = None;
    let (source, catalog): (
        Arc<dyn metriken_query::MetricsSource>,
        Arc<dyn ::dashboard::combined::ScopeCatalog>,
    ) = if following {
        let reader = Arc::new(rez::live::LiveReader::combined(
            path,
            combined.labels.clone(),
            combined.reader,
            Arc::clone(&pool),
        ));
        live = Some(Arc::clone(&reader));
        (reader.clone(), Arc::new(LiveCatalog(reader)))
    } else {
        let reader = Arc::new(combined.reader);
        (
            reader.clone(),
            Arc::new(::dashboard::combined::FixedCatalog(reader)),
        )
    };
    let state = AppState::with_pool(
        source,
        registry.clone(),
        config.template_preference,
        Arc::clone(&pool),
    );
    *state.parquet_path.write() = Some(path.to_path_buf());
    *state.baseline_name.write() = Some(path.display().to_string());
    *state.baseline_file.write() = state::BaselineFile::Combined(combined.labels.len());
    *state.selection.write() = selection;
    *state.trimmed_report_marker.write() = report_marker;
    state.captures.set_baseline_systeminfo(combined.systeminfo);
    state.captures.set_baseline_file_metadata(file_meta);
    state
        .captures
        .set_baseline_alias(config.baseline_alias.clone());
    *state.members.write() = combined.members;
    *state.combined_catalog.write() = Some(catalog);
    if let Some(live) = live {
        *state.combined_live.lock() = Some((Arc::clone(&live), 0));
        let followed = vec![(capture_registry::BASELINE_ID.to_string(), live)];
        match follow::Follow::start(followed, Arc::clone(&pool), follow::FOLLOW_INTERVAL) {
            Ok(f) => {
                info!(
                    "{} was not finalized when opened; following it",
                    path.display()
                );
                *state.follow.lock() = Some(f);
            }
            Err(e) => warn!(
                "could not follow {}: {e}; showing it as it was when opened",
                path.display()
            ),
        }
    }
    info!("Generating dashboards...");
    metadata::regenerate_dashboards(&state);
    state
}

/// Archive file mode: open each recording of the dendro archive at `path` as
/// a `RezReader` (a `MetricsSource`) and build the dashboards from them,
/// with each recording's metadata for its systeminfo and file metadata.
fn init_file_mode_rez(
    config: &Config,
    path: &Path,
    registry: &TemplateRegistry,
    pool: Arc<metriken_storage::BufferPool>,
) -> AppState {
    use metriken_query::MetricsSource;
    use std::collections::BTreeMap;
    info!("Loading data from {}...", path.display());

    // One reader per recording (labels preserved).
    let readers = crate::rez_reader::RezReader::open_recordings(path, Arc::clone(&pool))
        .unwrap_or_else(|e| {
            eprintln!("failed to load {}: {e}", path.display());
            std::process::exit(1);
        });
    if readers.is_empty() {
        eprintln!("{} holds no recordings", path.display());
        std::process::exit(1);
    }

    // A second file is the experiment of a two-file comparison: each file
    // is one recording.
    if let Some(experiment) = &config.experiment_path {
        if readers.len() != 1 {
            let file = path.display().to_string();
            eprintln!(
                "{}",
                ::rez::container::one_recording_hint(
                    env!("CARGO_BIN_NAME"),
                    &file,
                    "the baseline",
                    readers.len(),
                    &file,
                    &experiment.display().to_string(),
                    readers
                        .iter()
                        .any(|(l, _)| l.contains_key(::dashboard::combined::ARM_LABEL)),
                )
            );
            std::process::exit(1);
        }
    }

    // An archive of one run opens as one combined view; a comparison only
    // when --baseline/--experiment ask for one or the recordings say they
    // are one (`dashboard::combined::layout`).
    if opens_combined(&readers, config.selects_a_recording()) {
        return init_file_mode_combined(config, path, registry, pool, readers);
    }

    let all_labels: Vec<BTreeMap<String, String>> =
        readers.iter().map(|(l, _)| l.clone()).collect();
    let n = readers.len();

    let (b_idx, e_idx) = resolve_ab_slots(
        &all_labels,
        &config.baseline_recording,
        &config.experiment_recording,
    )
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    // The slots, then every other recording beside the comparison: one
    // without an arm, or one --baseline/--experiment did not name. Each is
    // shown in its own sections, as a single capture
    // (`dashboard::dashboard::add_beside_sections`), never as a third arm.
    let mut order: Vec<usize> = vec![b_idx];
    order.extend(e_idx);
    let beside_idx: Vec<usize> = (0..n).filter(|i| !order.contains(i)).collect();
    let beside_names = ::dashboard::dashboard::beside_names(
        &beside_idx
            .iter()
            .map(|&i| all_labels[i].clone())
            .collect::<Vec<_>>(),
        &beside_idx,
    );
    if !beside_idx.is_empty() {
        info!(
            "beside the comparison, each in its own sections: {}",
            beside_idx
                .iter()
                .map(|&i| crate::mcp::render_labels(&all_labels[i]))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    // Ids and aliases from the shared scheme, over the recordings AS SHOWN, so
    // one archive names its captures identically here and in the browser.
    let ordered_labels: Vec<BTreeMap<String, String>> =
        order.iter().map(|&i| all_labels[i].clone()).collect();
    let identities =
        dashboard::capture_alias::assign_capture_identities(&ordered_labels, &all_labels);

    // An archive that was not finalized when opened (a running hindsight
    // buffer, a `record` in progress, or a file whose writer is gone) is
    // followed: each recording shown is read through a `LiveReader` that a
    // `follow::Follow` thread reopens until the follow ends (see follow.rs).
    let mut following = order
        .iter()
        .chain(&beside_idx)
        .any(|&i| !readers[i].1.complete());
    // A reopen finds each recording by its label set, so a shown recording
    // whose label set another recording shares cannot be found again.
    if following {
        if let Some(&dup) = order
            .iter()
            .chain(&beside_idx)
            .find(|&&i| all_labels.iter().filter(|l| **l == all_labels[i]).count() > 1)
        {
            warn!(
                "{} is not finalized, but it holds several recordings labelled {}; \
                 showing it as it was when opened",
                path.display(),
                crate::mcp::render_labels(&all_labels[dup])
            );
            following = false;
        }
    }
    let mut followed: Vec<(String, Arc<rez::live::LiveReader>)> = Vec::new();
    let mut source = |id: &str,
                      labels: &BTreeMap<String, String>,
                      reader: crate::rez_reader::RezReader|
     -> Arc<dyn metriken_query::MetricsSource> {
        if !following {
            return Arc::new(reader);
        }
        let live = Arc::new(rez::live::LiveReader::from_reader(
            path,
            labels.clone(),
            reader,
            Arc::clone(&pool),
        ));
        followed.push((id.to_string(), Arc::clone(&live)));
        live
    };

    let mut slots: Vec<Option<(BTreeMap<String, String>, crate::rez_reader::RezReader)>> =
        readers.into_iter().map(Some).collect();

    // A recording that holds no rows renders an empty dashboard — the truthful
    // outcome for an endpoint scraped but silent, but it looks like a failed
    // load, so say which it is.
    let warn_if_empty = |labels: &BTreeMap<String, String>,
                         reader: &crate::rez_reader::RezReader| {
        if reader.is_empty() {
            warn!(
                "recording {} holds no rows; its dashboard will be empty",
                crate::mcp::render_labels(labels)
            );
        }
    };

    // A report's selection, marker and events: the baseline's, else those
    // of the first recording with a selection
    // (`dashboard::combined::report_markers`).
    let markers = {
        let shown: Vec<&dyn metriken_query::MetricsSource> = order
            .iter()
            .chain(&beside_idx)
            .filter_map(|&i| slots[i].as_ref())
            .map(|(_, r)| r as &dyn metriken_query::MetricsSource)
            .collect();
        ::dashboard::combined::report_markers(&shown)
    };
    let b_selection =
        ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_SELECTION);
    let b_report_marker =
        ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_REPORT);
    // The anchor (order[0]) becomes the AppState's baseline capture.
    let (b_labels, b_reader) = slots[order[0]].take().expect("anchor index is in range");
    warn_if_empty(&b_labels, &b_reader);
    let b_systeminfo = b_reader.metadata_get("systeminfo");
    let b_file_meta = file_meta_json(&b_reader)
        .map(|meta| ::dashboard::combined::with_report_markers(&meta, &markers));
    // A CLI --baseline-alias still wins over the label-derived one.
    let b_alias = config
        .baseline_alias
        .clone()
        .unwrap_or_else(|| identities[0].alias.clone());

    let primary = source(capture_registry::BASELINE_ID, &b_labels, b_reader);
    let mut beside = Vec::with_capacity(beside_idx.len());
    for (&idx, (key, name)) in beside_idx.iter().zip(beside_names) {
        let (labels, reader) = slots[idx].take().expect("index is in range");
        warn_if_empty(&labels, &reader);
        let data = source(&format!("beside:{key}"), &labels, reader);
        beside.push(::dashboard::dashboard::Beside { key, name, data });
    }
    let baseline: Arc<dyn metriken_query::MetricsSource> = if beside.is_empty() {
        primary
    } else {
        Arc::new(::dashboard::combined::Routed::new(
            primary,
            beside
                .iter()
                .map(|b| (b.key.clone(), Arc::clone(&b.data)))
                .collect(),
        ))
    };
    let state = AppState::with_pool(
        baseline,
        registry.clone(),
        config.template_preference,
        Arc::clone(&pool),
    );
    let b_systeminfo =
        ::dashboard::dashboard::with_beside_systeminfo(b_systeminfo, &b_alias, &beside);
    let b_file_meta =
        b_file_meta.map(|meta| ::dashboard::dashboard::with_beside_metadata(&meta, &beside));
    *state.beside.write() = beside;
    *state.anchor_labels.write() = Some(b_labels.clone());
    *state.parquet_path.write() = Some(path.to_path_buf());
    *state.baseline_name.write() = Some(path.display().to_string());
    *state.baseline_file.write() = match n {
        1 => state::BaselineFile::One,
        n => state::BaselineFile::Comparison(n),
    };
    *state.selection.write() = b_selection;
    // A saved report stamps `KEY_REPORT=trimmed` on its anchor recording.
    // Without this a reloaded report would render its (mostly trimmed)
    // sections instead of going straight to the saved Report view.
    *state.trimmed_report_marker.write() = b_report_marker;
    state.captures.set_baseline_systeminfo(b_systeminfo);
    state.captures.set_baseline_file_metadata(b_file_meta);
    state.captures.set_baseline_alias(Some(b_alias));

    // The experiment attaches under its own id.
    for (pos, &idx) in order.iter().enumerate().skip(1) {
        let (labels, reader) = slots[idx].take().expect("index is in range");
        warn_if_empty(&labels, &reader);
        let systeminfo = reader.metadata_get("systeminfo");
        let file_meta = file_meta_json(&reader);
        state.captures.attach_capture(
            &identities[pos].id,
            source(&identities[pos].id, &labels, reader),
            systeminfo,
            file_meta,
            Some(identities[pos].alias.clone()),
        );
    }

    if following {
        match follow::Follow::start(followed, Arc::clone(&pool), follow::FOLLOW_INTERVAL) {
            Ok(f) => {
                info!(
                    "{} was not finalized when opened; following it",
                    path.display()
                );
                *state.follow.lock() = Some(f);
            }
            Err(e) => warn!(
                "could not follow {}: {e}; showing it as it was when opened",
                path.display()
            ),
        }
    }

    if let Some(exp_path) = &config.experiment_path {
        let baseline = path.display().to_string();
        let name = exp_path.display().to_string();
        let (reader, systeminfo, file_meta) = open_experiment(
            exp_path,
            &name,
            &baseline,
            baseline_has_arms(&state),
            Arc::clone(&pool),
        )
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        state.captures.attach_experiment(
            Arc::new(reader) as Arc<dyn metriken_query::MetricsSource>,
            systeminfo,
            file_meta,
            config.experiment_alias.clone(),
        );
        *state.cli_experiment_path.write() = Some(exp_path.clone());
        info!("Attached experiment capture: {}", exp_path.display());
    }

    if let Some(ref cat_name) = config.category_name {
        validate_category_at_startup(&state, registry, cat_name);
    }

    info!("Generating dashboards...");
    metadata::regenerate_dashboards(&state);
    state
}

/// Validate `--category`: the named category must exist, and the sources of
/// both captures' templates must appear in its `members`. Refuses to launch
/// on misconfiguration; silent fall-back hides user intent.
fn validate_category_at_startup(state: &AppState, registry: &TemplateRegistry, cat_name: &str) {
    let category = registry.get_category(cat_name).unwrap_or_else(|| {
        eprintln!("no category template named {cat_name:?} found in the registry");
        std::process::exit(1);
    });
    let Some(experiment) = state.captures.get(CaptureId::Experiment) else {
        eprintln!("--category {cat_name:?} requires both a baseline and an experiment capture");
        std::process::exit(1);
    };
    let sources_of = |data: &dyn metriken_query::MetricsSource| -> Vec<String> {
        metadata::capture_service_extensions(data, registry, state.template_preference)
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    };
    let baseline_sources = sources_of(state.baseline_data().as_ref());
    let experiment_sources = sources_of(experiment.as_ref());
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

fn init_live_mode(
    rt: &tokio::runtime::Runtime,
    url: &Url,
    registry: &TemplateRegistry,
    template_preference: TemplatePreference,
    pool: Arc<metriken_storage::BufferPool>,
) -> AppState {
    info!("Connecting to live agent at {url}...");
    let (info, session) = rt.block_on(async {
        let client = Client::builder()
            .http1_only()
            .build()
            .expect("failed to create http client");
        let info = match actions::fetch_agent_info(&client, url).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        match live::LiveSession::start(&client, url, info.clone(), Arc::clone(&pool)).await {
            Ok(session) => (info, session),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    });
    info!(
        "Recording {source} {version} at {url} from its replication stream",
        source = info.source,
        version = info.version
    );

    let state = AppState::with_pool(
        session.reader(),
        registry.clone(),
        template_preference,
        pool,
    );
    let context = metadata::live_dashboard_context(session.reader().as_ref(), template_preference);
    *state.sections.write() = state::LazySectionStore::new(context);
    state.install_live(session);
    state.live.store(true, Ordering::Relaxed);
    state.captures.set_baseline_systeminfo(info.sysinfo);

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

    /// `init_file_mode` opens a v3 `.rez` upgraded to `.dendro`.
    #[test]
    fn init_file_mode_reads_an_upgraded_rez() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let rez_path = dir.path().join("v3.dendro");
        crate::parquet_tools::legacy::convert(&legacy, &rez_path).unwrap();

        let matches = super::command().get_matches_from(["view", rez_path.to_str().unwrap()]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);

        let state = super::init_file_mode(&config, &rez_path, &registry, pool);
        assert_eq!(*state.parquet_path.read(), Some(rez_path));
        // One recording is not confusable with anything, so it keeps the
        // positional name — a hindsight snapshot's `{source: rezolus}` is not
        // a useful title.
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::BASELINE_ID)
                .as_deref(),
            Some("baseline"),
        );
    }

    /// Two one-recording archives open as a two-file comparison, the
    /// second in the experiment slot; an experiment of several recordings is
    /// refused naming `recording combine`, and a legacy one naming the upgrade.
    #[test]
    fn a_second_file_is_a_one_recording_experiment() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.dendro"), dir.path().join("b.dendro"));
        ::rez::fixtures::multi_recording(&a, &[("redis", "web-01")]);
        ::rez::fixtures::multi_recording(&b, &[("valkey", "web-02")]);
        let matches = super::command().get_matches_from([
            "view",
            a.to_str().unwrap(),
            &format!("after={}", b.display()),
        ]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, &a, &registry, pool);
        assert!(state.captures.experiment_attached());
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::EXPERIMENT_ID)
                .as_deref(),
            Some("after")
        );
        let experiment = state
            .captures
            .get(crate::viewer::capture_registry::CaptureId::Experiment)
            .unwrap();
        assert_eq!(experiment.metadata_get("source").as_deref(), Some("valkey"));

        let pool = || metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let two = dir.path().join("two.dendro");
        ::rez::fixtures::multi_recording(&two, &[("redis", "web-01"), ("valkey", "web-02")]);
        let err = super::open_experiment(
            &two,
            &two.display().to_string(),
            "one.dendro",
            false,
            pool(),
        )
        .err()
        .unwrap();
        let combine = format!(
            "recording combine one.dendro {} -o both.dendro",
            two.display()
        );
        assert!(
            err.contains("holds 2 recordings") && err.contains(&combine),
            "{err}"
        );
        let legacy = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let err = super::open_experiment(&legacy, "v3.rez", "one.dendro", false, pool())
            .err()
            .unwrap();
        assert!(err.contains("rezolus recording upgrade"), "{err}");
    }

    /// Selection and events in a recording's metadata are surfaced by
    /// `init_file_mode_rez`: selection into `state.selection`, events inside
    /// the baseline's `file_metadata` (where the frontend's
    /// `seedEventsFromMetadata` finds them).
    #[test]
    fn init_file_mode_rez_surfaces_manifest_selection_and_events() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = ::rez::fixtures::committed_rez("v3.rez", dir.path());

        // A selection + events payload in the recording's manifest metadata
        // (the shape `annotate`/report-save produced in a `.rez`).
        ::rez::fixtures::add_rez_metadata(
            &legacy,
            0,
            &[
                (
                    crate::parquet_metadata::KEY_SELECTION,
                    r#"{"entries":[{"query":"cpu_usage"}]}"#,
                ),
                (
                    crate::parquet_metadata::KEY_EVENTS,
                    r#"{"events":[{"timestamp":1000000000,"description":"rollout"}]}"#,
                ),
                (
                    crate::parquet_metadata::KEY_REPORT,
                    crate::parquet_metadata::REPORT_VALUE_TRIMMED,
                ),
            ],
        );

        let rez_path = dir.path().join("v3.dendro");
        crate::parquet_tools::legacy::convert(&legacy, &rez_path).unwrap();
        let matches = super::command().get_matches_from(["view", rez_path.to_str().unwrap()]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, &rez_path, &registry, pool);

        // Selection reached the global slot the `/api/v1/selection` route serves.
        assert!(
            state
                .selection
                .read()
                .as_deref()
                .is_some_and(|s| s.contains("cpu_usage")),
            "the baseline recording's selection must fill state.selection"
        );

        // The report marker on the anchor makes the archive load as a report —
        // the round trip Save-as-Report depends on.
        assert!(
            state.is_trimmed_report(),
            "KEY_REPORT on the anchor manifest must set the trimmed-report marker"
        );

        // Events ride the baseline's file_metadata blob, parsed as JSON.
        let fm = state
            .captures
            .file_metadata_by_id(crate::viewer::capture_registry::BASELINE_ID)
            .expect("baseline file_metadata is present");
        let fm: serde_json::Value = serde_json::from_str(&fm).unwrap();
        assert_eq!(
            fm["events"]["events"][0]["description"], "rollout",
            "events must be nested (as parsed JSON) under the `events` key of file_metadata"
        );
    }

    /// A one-recording `.dendro` whose source is `source`, with `extra` added
    /// to its source metadata.
    fn dendro_with_metadata(
        dir: &std::path::Path,
        source: &str,
        extra: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let path = dir.join("rec.dendro");
        let mut sources = ::rez::fixtures::multi_recording_sources(&[(source, "web-01")]);
        sources[0].metadata.extend(::rez::fixtures::map(extra));
        ::rez::fixtures::write_dendro(&path, &sources, None, ::rez::fixtures::Finish::Finalized);
        path
    }

    /// A one-KPI template whose KPI title names where it came from.
    fn titled_template(service: &str, title: &str) -> String {
        serde_json::json!({
            "service_name": service,
            "kpis": [{"role": "throughput", "title": title, "query": "up", "type": "gauge"}],
        })
        .to_string()
    }

    /// The (source, first KPI title) pairs and section routes the viewer built
    /// for `path` with the CLI flags `flags`, against a registry that knows
    /// two services, `known` and `other`.
    fn served_templates(
        path: &std::path::Path,
        flags: &[&str],
    ) -> (Vec<(String, String)>, Vec<String>) {
        let mut argv = vec!["view", path.to_str().unwrap()];
        argv.extend_from_slice(flags);
        let config = super::Config::try_from(super::command().get_matches_from(argv)).unwrap();
        let registry = ::dashboard::TemplateRegistry::from_templates(
            ["known", "other"]
                .iter()
                .map(|s| serde_json::from_str(&titled_template(s, "from the registry")).unwrap())
                .collect(),
        );
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, path, &registry, pool);
        let sections = state.sections.read();
        let exts = sections
            .context()
            .service_exts
            .iter()
            .map(|(s, e)| (s.clone(), e.kpis[0].title.clone()))
            .collect();
        let routes = sections
            .sections()
            .iter()
            .map(|s| s.route.clone())
            .collect();
        (exts, routes)
    }

    /// A dendro source carrying a template for a service the registry does
    /// not know renders that template's section.
    #[test]
    fn a_dendro_sources_own_template_renders_for_an_unknown_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dendro_with_metadata(
            dir.path(),
            "acme",
            &[("service_queries", &titled_template("acme", "carried"))],
        );
        let (exts, routes) = served_templates(&path, &[]);
        assert_eq!(exts, [("acme".to_string(), "carried".to_string())]);
        assert!(
            routes.iter().any(|r| r == "/service/acme"),
            "the carried template's section is listed: {routes:?}"
        );
    }

    /// For a service the registry knows, the archive's template wins, and
    /// `--registry-templates` renders the registry's instead.
    #[test]
    fn the_archives_template_wins_unless_the_registry_is_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let path = dendro_with_metadata(
            dir.path(),
            "known",
            &[("service_queries", &titled_template("known", "carried"))],
        );
        let (exts, _) = served_templates(&path, &[]);
        assert_eq!(exts, [("known".to_string(), "carried".to_string())]);
        let (exts, _) = served_templates(&path, &["--registry-templates"]);
        assert_eq!(
            exts,
            [("known".to_string(), "from the registry".to_string())]
        );
    }

    /// A template in the metadata that does not parse falls back to the
    /// registry's for that source rather than failing the load.
    #[test]
    fn a_malformed_carried_template_falls_back_to_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dendro_with_metadata(
            dir.path(),
            "known",
            &[("service_queries", r#"{"service_name": 7}"#)],
        );
        let (exts, _) = served_templates(&path, &[]);
        assert_eq!(
            exts,
            [("known".to_string(), "from the registry".to_string())]
        );
    }

    /// A recording whose `source` is a JSON array (the shape a combined
    /// parquet's footer has, here in a dendro source's metadata) with a
    /// template annotated at the top level: the template renders under its
    /// own `service_name`, replacing that source's registry entry, and the
    /// other sources keep the registry's. No section is named after the
    /// array.
    #[test]
    fn a_top_level_template_on_a_combined_source_is_keyed_by_its_service_name() {
        let dir = tempfile::tempdir().unwrap();
        let psm = serde_json::json!({
            "known": {"0": {"version": "1"}},
            "other": {"0": {"version": "1"}},
            "rezolus": {"0": {"version": "6"}},
        })
        .to_string();
        let path = dendro_with_metadata(
            dir.path(),
            "combined",
            &[
                ("source", r#"["known","other","rezolus"]"#),
                ("per_source_metadata", &psm),
                ("service_queries", &titled_template("known", "carried")),
            ],
        );
        let (exts, routes) = served_templates(&path, &[]);
        assert_eq!(
            exts,
            [
                ("known".to_string(), "carried".to_string()),
                ("other".to_string(), "from the registry".to_string()),
            ]
        );
        assert!(
            !routes.iter().any(|r| r.contains('[')),
            "no section is named after the source array: {routes:?}"
        );
        let (exts, _) = served_templates(&path, &["--registry-templates"]);
        assert_eq!(
            exts,
            [
                ("known".to_string(), "from the registry".to_string()),
                ("other".to_string(), "from the registry".to_string()),
            ]
        );
    }

    /// An archive's simple capture (a source with no template and no
    /// Rezolus metrics) is classified from its recording's metadata, so its
    /// section is named by its `source`, as the WASM viewer names it, rather
    /// than by the archive's filename.
    #[test]
    fn an_archive_simple_capture_is_named_by_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("simple_capture.dendro");
        ::rez::fixtures::multi_recording(&path, &[("127.0.0.1-18651", "web-01")]);
        let (exts, routes) = served_templates(&path, &[]);
        assert!(exts.is_empty(), "{exts:?}");
        assert!(
            routes.iter().any(|r| r == "/source/127.0.0.1-18651"),
            "{routes:?}"
        );
        assert!(
            !routes.iter().any(|r| r == "/source/simple_capture"),
            "{routes:?}"
        );
    }

    /// Each recording of a two-recording archive renders the template its own
    /// metadata carries: the experiment's is read from its recording.
    #[test]
    fn each_recording_of_an_archive_renders_its_own_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.dendro");
        let mut sources =
            ::rez::fixtures::multi_recording_sources(&[("alpha", "web-01"), ("beta", "web-01")]);
        for source in &mut sources {
            let name = source.labels["source"].clone();
            source.metadata.insert(
                "service_queries".to_string(),
                titled_template(&name, &format!("{name}'s own")),
            );
        }
        ::rez::fixtures::write_dendro(&path, &sources, None, ::rez::fixtures::Finish::Finalized);
        let (exts, _) = served_templates(&path, &[]);
        assert_eq!(
            exts,
            [
                ("alpha".to_string(), "alpha's own".to_string()),
                ("beta".to_string(), "beta's own".to_string()),
            ]
        );
    }

    /// Labels for a fixture archive, in manifest order.
    fn labels(pairs: &[(&str, &str)]) -> Vec<std::collections::BTreeMap<String, String>> {
        pairs
            .iter()
            .map(|(source, host)| {
                [
                    ("source".to_string(), source.to_string()),
                    ("host".to_string(), host.to_string()),
                ]
                .into_iter()
                .collect()
            })
            .collect()
    }

    fn sel(flag: &str, pairs: &[&str]) -> crate::mcp::RecordingSelector {
        crate::mcp::RecordingSelector::parse(flag, pairs.iter().map(|s| s.to_string())).unwrap()
    }

    fn none() -> crate::mcp::RecordingSelector {
        crate::mcp::RecordingSelector::default()
    }

    /// The historical default survives: no selectors means recordings 0 and 1.
    #[test]
    fn with_no_selectors_the_first_two_recordings_fill_the_slots() {
        let all = labels(&[
            ("redis", "web-01"),
            ("valkey", "web-01"),
            ("envoy", "web-02"),
        ]);
        assert_eq!(
            super::resolve_ab_slots(&all, &none(), &none()).unwrap(),
            (0, Some(1))
        );
        assert_eq!(
            super::resolve_ab_slots(&all[..1], &none(), &none()).unwrap(),
            (0, None),
            "a single-recording archive has no experiment slot"
        );
    }

    /// The point of the flags: on a 3-recording archive, pick the two that
    /// are NOT the first two.
    #[test]
    fn selectors_choose_recordings_the_default_would_not_show() {
        let all = labels(&[
            ("redis", "web-01"),
            ("valkey", "web-01"),
            ("envoy", "web-02"),
        ]);
        assert_eq!(
            super::resolve_ab_slots(
                &all,
                &sel("--baseline", &["source=envoy"]),
                &sel("--experiment", &["source=valkey"]),
            )
            .unwrap(),
            (2, Some(1))
        );
    }

    /// Several pairs are ANDed, as a subset match — `host=web-01` alone is
    /// ambiguous here, `host=web-01 source=valkey` is not.
    #[test]
    fn selector_labels_are_anded_as_a_subset_match() {
        let all = labels(&[
            ("redis", "web-01"),
            ("valkey", "web-01"),
            ("envoy", "web-02"),
        ]);
        assert_eq!(
            super::resolve_ab_slots(
                &all,
                &sel("--baseline", &["host=web-01", "source=valkey"]),
                &none(),
            )
            .unwrap(),
            (1, None)
        );
    }

    /// A baseline selector on its own shows that one recording. Pairing it
    /// with whatever recording happens to sit next to it would invent the
    /// other half of a comparison nobody asked for.
    #[test]
    fn a_baseline_selector_alone_shows_one_recording_and_invents_no_peer() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        assert_eq!(
            super::resolve_ab_slots(&all, &sel("--baseline", &["source=redis"]), &none()).unwrap(),
            (0, None),
        );
    }

    /// `--experiment` alone would leave the baseline slot defaulting to
    /// recording 0 — possibly the very recording named, i.e. a comparison of
    /// something against itself.
    #[test]
    fn an_experiment_selector_without_a_baseline_is_refused() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        let err = super::resolve_ab_slots(&all, &none(), &sel("--experiment", &["source=redis"]))
            .unwrap_err();
        assert!(
            err.contains("--baseline"),
            "the error must say what is missing: {err}"
        );
        assert!(
            err.contains("source=redis") && err.contains("source=valkey"),
            "and list what could fill it: {err}"
        );
    }

    /// Both selectors landing on one recording is not a comparison. Reachable
    /// without a typo: the matches are subset matches, so `--baseline
    /// host=web-01 source=redis` and `--experiment source=redis` name the
    /// same arm.
    #[test]
    fn both_selectors_naming_one_recording_is_refused() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        let err = super::resolve_ab_slots(
            &all,
            &sel("--baseline", &["host=web-01", "source=redis"]),
            &sel("--experiment", &["source=redis"]),
        )
        .unwrap_err();
        assert!(
            err.contains("same recording") && err.contains("source=redis"),
            "the error must name the recording both landed on: {err}"
        );
    }

    /// A selector matching nothing is refused with the archive's contents —
    /// never narrowed to a first match, which would render one arm under the
    /// other's name.
    #[test]
    fn a_selector_matching_nothing_lists_the_candidates() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        let err = super::resolve_ab_slots(&all, &sel("--baseline", &["source=nope"]), &none())
            .unwrap_err();
        assert!(err.contains("matched no recording"), "{err}");
        assert!(
            err.contains("source=redis") && err.contains("source=valkey"),
            "{err}"
        );
    }

    /// An ambiguous selector lists the ones it matched, not the whole archive.
    #[test]
    fn an_ambiguous_selector_lists_the_recordings_it_matched() {
        let all = labels(&[
            ("redis", "web-01"),
            ("valkey", "web-01"),
            ("envoy", "web-02"),
        ]);
        let err = super::resolve_ab_slots(&all, &sel("--baseline", &["host=web-01"]), &none())
            .unwrap_err();
        assert!(err.contains("matched 2 recordings"), "{err}");
        assert!(
            err.contains("source=redis") && err.contains("source=valkey"),
            "{err}"
        );
        assert!(
            !err.contains("source=envoy"),
            "an ambiguity listing describes the matches, not the archive: {err}"
        );
    }

    /// Every "select with:" line an error prints has to name the flag whose
    /// slot it fills — a viewer message advertising the MCP `--recording`
    /// flag is advice `rezolus view` rejects.
    #[test]
    fn error_listings_advertise_the_viewers_own_flags() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        for err in [
            super::resolve_ab_slots(&all, &sel("--baseline", &["source=nope"]), &none())
                .unwrap_err(),
            super::resolve_ab_slots(
                &all,
                &sel("--baseline", &["source=redis"]),
                &sel("--experiment", &["source=nope"]),
            )
            .unwrap_err(),
            super::resolve_ab_slots(&all, &none(), &sel("--experiment", &["source=redis"]))
                .unwrap_err(),
        ] {
            assert!(
                !err.contains("--recording"),
                "the viewer has no --recording flag: {err}"
            );
        }

        // And the slot that failed is the slot named.
        let err = super::resolve_ab_slots(
            &all,
            &sel("--baseline", &["source=redis"]),
            &sel("--experiment", &["source=nope"]),
        )
        .unwrap_err();
        assert!(
            err.contains("--experiment source=nope"),
            "the echoed selector must name the flag it arrived on: {err}"
        );
    }

    /// Every selector an error tells the caller to try has to parse back
    /// through the flag it is printed for. `Display`'s comma-joined form does
    /// not: `parse` splits on the first `=` only.
    #[test]
    fn every_selector_an_error_offers_parses_back() {
        let all = labels(&[("redis", "web-01"), ("valkey", "web-01")]);
        let err = super::resolve_ab_slots(&all, &sel("--baseline", &["source=nope"]), &none())
            .unwrap_err();
        let offered: Vec<&str> = err
            .lines()
            .filter_map(|l| l.trim().strip_prefix("select with: "))
            .collect();
        assert_eq!(offered.len(), 2, "one per recording: {err}");
        for line in offered {
            let pairs = line
                .strip_prefix("--baseline ")
                .unwrap_or_else(|| panic!("not a --baseline selector: {line}"))
                .split(" --baseline ")
                .map(str::to_string);
            let parsed = crate::mcp::RecordingSelector::parse("--baseline", pairs)
                .unwrap_or_else(|e| panic!("offered selector {line:?} does not parse: {e}"));
            super::resolve_ab_slots(&all, &parsed, &none())
                .unwrap_or_else(|e| panic!("offered selector {line:?} does not resolve: {e}"));
        }
    }

    /// The flags are only meaningful inside an archive; anywhere else they
    /// must be refused rather than ignored, since ignoring one loads a file
    /// the caller narrowed and shows it as the narrowed view.
    #[test]
    fn selectors_outside_an_archive_are_refused_by_name() {
        let msg = super::selectors_need_an_archive("upload-only mode");
        assert!(
            msg.contains("--baseline") && msg.contains("--experiment"),
            "{msg}"
        );
        assert!(msg.contains("upload-only mode"), "{msg}");
    }

    /// Selecting recordings must actually put THOSE recordings in the slots,
    /// asserted by each one's own metadata: a viewer that showed the first two
    /// regardless would still produce two populated dashboards.
    #[test]
    fn selected_recordings_fill_the_ab_slots() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("fleet.dendro");
        ::rez::fixtures::multi_recording(
            &rez_path,
            // One host, three services: `source` is what tells them apart, so
            // the aliases below are the selected recordings' own sources.
            &[
                ("redis", "web-01"),
                ("valkey", "web-01"),
                ("envoy", "web-01"),
            ],
        );

        let matches = super::command().get_matches_from([
            "view",
            rez_path.to_str().unwrap(),
            "--baseline",
            "source=envoy",
            "--experiment",
            "source=valkey",
        ]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, &rez_path, &registry, pool);

        assert!(state.captures.experiment_attached());
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::BASELINE_ID)
                .as_deref(),
            Some("envoy"),
        );
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::EXPERIMENT_ID)
                .as_deref(),
            Some("valkey"),
        );
        // Aliases come from the labels; the per-recording metadata is written
        // by a different path, so it is an independent witness that the right
        // reader landed in each slot.
        let b_meta = state
            .captures
            .file_metadata_by_id("baseline")
            .unwrap_or_default();
        let e_meta = state
            .captures
            .file_metadata_by_id("experiment")
            .unwrap_or_default();
        assert!(b_meta.contains("envoy"), "baseline metadata: {b_meta}");
        assert!(e_meta.contains("valkey"), "experiment metadata: {e_meta}");
    }

    /// A baseline selector alone loads one capture and attaches no experiment.
    #[test]
    fn a_baseline_selector_alone_leaves_the_experiment_slot_empty() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("fleet.dendro");
        ::rez::fixtures::multi_recording(&rez_path, &[("redis", "web-01"), ("valkey", "web-01")]);

        let matches = super::command().get_matches_from([
            "view",
            rez_path.to_str().unwrap(),
            "--baseline",
            "source=valkey",
        ]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, &rez_path, &registry, pool);

        assert!(!state.captures.experiment_attached());
        let b_meta = state
            .captures
            .file_metadata_by_id("baseline")
            .unwrap_or_default();
        assert!(b_meta.contains("valkey"), "baseline metadata: {b_meta}");
        // Named, not "baseline": the window title is the archive's filename,
        // so a generic alias would leave nothing on screen saying which of the
        // archive's recordings is being read.
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::BASELINE_ID)
                .as_deref(),
            Some("valkey"),
        );
    }

    /// Labels for a fixture recording.
    fn label_map(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Without flags, a comparison of three recordings carrying `arm` has
    /// more arms than slots: refused, listing the recordings and naming the
    /// flags that choose two.
    #[test]
    fn without_flags_three_armed_recordings_are_refused() {
        let all = ["redis", "valkey", "envoy"]
            .map(|s| label_map(&[("source", s), ("host", "web-01"), ("arm", s)]));
        let err = super::resolve_ab_slots(&all, &none(), &none()).unwrap_err();
        assert!(err.contains("3 recordings carry an `arm`"), "{err}");
        assert!(
            err.contains("--baseline") && err.contains("source=envoy"),
            "{err}"
        );
    }

    /// A label that cannot tell the whole archive apart can still tell the
    /// chosen pair apart. Scoping the alias choice to the archive would alias
    /// this pair "baseline"/"experiment" despite `host` naming them exactly.
    #[test]
    fn aliases_come_from_the_recordings_shown_not_the_whole_archive() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("fleet.dendro");
        ::rez::fixtures::multi_recording(
            &rez_path,
            // `host` does not discriminate all three (web-01 twice) and
            // neither does `source` (redis twice) — but it discriminates the
            // two selected below.
            &[
                ("redis", "web-01"),
                ("redis", "web-02"),
                ("valkey", "web-01"),
            ],
        );

        let matches = super::command().get_matches_from([
            "view",
            rez_path.to_str().unwrap(),
            "--baseline",
            "source=redis",
            "--baseline",
            "host=web-01",
            "--experiment",
            "source=redis",
            "--experiment",
            "host=web-02",
        ]);
        let config = super::Config::try_from(matches).unwrap();
        let registry = ::dashboard::TemplateRegistry::empty();
        let pool = metriken_storage::BufferPool::new(8 * 1024 * 1024);
        let state = super::init_file_mode(&config, &rez_path, &registry, pool);

        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::BASELINE_ID)
                .as_deref(),
            Some("web-01"),
        );
        assert_eq!(
            state
                .captures
                .alias_by_id(crate::viewer::capture_registry::EXPERIMENT_ID)
                .as_deref(),
            Some("web-02"),
        );
    }

    /// A repeated key on one flag is a contradiction, not a last-one-wins.
    #[test]
    fn a_repeated_selector_key_is_rejected_at_parse() {
        let matches = super::command().get_matches_from([
            "view",
            "out.rez",
            "--baseline",
            "source=redis",
            "--baseline",
            "source=valkey",
        ]);
        let err = match super::Config::try_from(matches) {
            Err(e) => e,
            Ok(_) => panic!("a repeated selector key must not be accepted"),
        };
        assert!(err.contains("--baseline"), "{err}");
        assert!(err.contains("more than once"), "{err}");
    }

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
        assert!(payload.get("num_series").is_none());
    }

    #[test]
    fn lazy_section_store_exposes_context_sections() {
        let context = DashboardContext {
            sections: vec![
                Section {
                    name: "Overview".to_string(),
                    route: "/overview".to_string(),
                    instance: None,
                    landing: false,
                },
                Section {
                    name: "CPU".to_string(),
                    route: "/cpu".to_string(),
                    instance: None,
                    landing: false,
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
