use super::*;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use metriken_exposition::Snapshot;
use metriken_exposition::SnapshotV2;
use metriken_exposition::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::SystemTime;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::decompression::RequestDecompressionLayer;

static SNAPSHOT: Mutex<Option<SnapshotV2>> = Mutex::new(None);

mod config;
mod prometheus;
mod snapshot;

pub use config::Config;
use prometheus::prometheus;
use snapshot::snapshot;

pub fn command() -> Command {
    Command::new("exporter")
        .about("Exposition of metrics from a Rezolus agent")
        .arg(
            clap::Arg::new("CONFIG")
                .help("Rezolus exporter configuration file")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
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

    let level = config.log().level();

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

    // our http client
    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    // launch the HTTP listener
    let c = config.clone();
    rt.spawn(async move { serve(c).await });

    // timed loop to calculate summary metrics
    rt.block_on(async move {
        // sampling interval
        let mut interval = crate::common::aligned_interval(config.general().interval().into());

        // previous snapshot
        let mut previous = None;

        let url = config.general().mpk_url();

        // sample in a loop
        loop {
            // wait to sample
            interval.tick().await;

            let start = Instant::now();

            // sample rezolus
            if let Ok(response) = client.get(url.clone()).send().await {
                if let Ok(body) = response.bytes().await {
                    let latency = start.elapsed();

                    debug!("sampling latency: {} us", latency.as_micros());

                    let mut reader = std::io::Cursor::new(body.as_ref());

                    if let Ok(current) =
                        rmp_serde::from_read::<&mut std::io::Cursor<&[u8]>, Snapshot>(&mut reader)
                    {
                        if let Some(previous) = previous.take() {
                            let snapshot = snapshot(&config, previous, current.clone(), latency);

                            let mut s = SNAPSHOT.lock();
                            *s = Some(snapshot);
                        }

                        previous = Some(current);
                    }
                }
            }
        }
    });

    std::thread::sleep(Duration::from_millis(200));
}

async fn serve(config: Arc<Config>) {
    let app: Router = app(config.clone());

    let listener = TcpListener::bind(config.general().listen())
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

struct AppState {
    client: Client,
    mpk_url: Url,
    json_url: Url,
}

fn app(config: Arc<Config>) -> Router {
    let mpk_url = config.general().mpk_url();
    let json_url = config.general().json_url();

    let state = Arc::new(AppState {
        client: Client::builder().http1_only().build().unwrap(),
        mpk_url,
        json_url,
    });

    Router::new()
        .route("/", get(root))
        .route("/metrics", get(prometheus))
        .route("/metrics/binary", get(msgpack))
        .route("/metrics/json", get(json))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Exporter\nFor information, see: https://rezolus.com\n")
}

// for convenience, this proxies the msgpack from Rezolus Agent
async fn msgpack(State(state): State<Arc<AppState>>) -> Vec<u8> {
    if let Ok(response) = state.client.get(state.mpk_url.clone()).send().await {
        if let Ok(body) = response.bytes().await {
            return body.to_vec();
        }
    }

    Vec::new()
}

async fn json(State(state): State<Arc<AppState>>) -> String {
    if let Ok(response) = state.client.get(state.json_url.clone()).send().await {
        if let Ok(body) = response.bytes().await {
            if let Ok(s) = std::str::from_utf8(&body) {
                return s.to_string();
            }
        }
    }

    String::new()
}
