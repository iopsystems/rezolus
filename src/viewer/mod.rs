use super::*;

#[cfg(not(feature = "developer-mode"))]
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::ArgMatches;
use http::StatusCode;
#[cfg(not(feature = "developer-mode"))]
use http::{header, Uri};
#[cfg(not(feature = "developer-mode"))]
use include_dir::{include_dir, Dir};
use serde::Serialize;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;
use tower_livereload::LiveReloadLayer;

use std::collections::HashMap;
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
pub mod promql;
pub mod tsdb;

use plot::*;
use promql::QueryEngine;
use tsdb::*;

pub fn command() -> Command {
    Command::new("view")
        .about("View a Rezolus artifact")
        .arg(
            clap::Arg::new("INPUT")
                .help("Rezolus parquet file")
                .value_parser(value_parser!(PathBuf))
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
    input: PathBuf,
    verbose: u8,
    listen: SocketAddr,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        Ok(Config {
            input: args.get_one::<PathBuf>("INPUT").unwrap().to_path_buf(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            listen: *args
                .get_one::<SocketAddr>("LISTEN")
                .unwrap_or(&"127.0.0.1:0".to_socket_addrs().unwrap().next().unwrap()),
        })
    }
}

/// Runs the Rezolus exporter tool which is a Rezolus client that pulls data
/// from the msgpack endpoint and exports summary metrics on a Prometheus
/// compatible metrics endpoint. This allows for direct collection of percentile
/// metrics and/or full histograms with counter and gauge metrics passed through
/// directly.
pub fn run(config: Config) {
    // load config from file
    let config: Arc<Config> = config.into();

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match config.verbose {
        0 => Level::Info,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    let debug_log = if level <= Level::Info {
        LogBuilder::new().format(ringlog::default_format)
    } else {
        LogBuilder::new()
    }
    .output(debug_output)
    .build()
    .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(level.to_level_filter())
        .default(debug_log)
        .build()
        .start();

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    ctrlc::set_handler(move || {
        std::process::exit(2);
    })
    .expect("failed to set ctrl-c handler");

    info!("Loading data from parquet file...");
    let data = Tsdb::load(&config.input)
        .map_err(|e| {
            eprintln!("failed to load data from parquet: {e}");
            std::process::exit(1);
        })
        .unwrap();

    info!("Generating dashboards...");
    let data_arc = Arc::new(data);
    let state = dashboard::generate(data_arc);

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
    query_engine: Arc<QueryEngine>,
}

impl AppState {
    pub fn new(tsdb: Arc<Tsdb>) -> Self {
        Self {
            sections: Default::default(),
            query_engine: Arc::new(QueryEngine::new(tsdb)),
        }
    }
}

// NOTE: we're going to want to include the assets in the binary for release
// builds. For now, we just serve from the assets folder
fn app(livereload: LiveReloadLayer, state: AppState) -> Router {
    let state = Arc::new(state);

    let router = Router::new()
        .route("/about", get(about))
        .route("/data/{path}", get(data))
        .with_state(state.clone())
        .merge(promql::routes(state.query_engine.clone()));

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
