use super::*;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use metriken_exposition::Snapshot;
use metriken_exposition::SnapshotV2;
use metriken_exposition::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::Duration;
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
        .about("Serve a Rezolus agent's metrics on a Prometheus-compatible endpoint")
        .long_about(
            "Long-running service that pulls from a Rezolus agent's msgpack endpoint and\n\
             re-exposes the metrics in Prometheus text format for a Prometheus/VictoriaMetrics\n\
             scraper to collect.\n\n\
             Configuration is a TOML file (the only argument). It sets the agent to read from\n\
             ([general] source, default 0.0.0.0:4241 — the shipped config sets\n\
             127.0.0.1:4241), the address to serve `/metrics` on\n\
             ([general] listen, default 0.0.0.0:4242), and whether to expose full histogram\n\
             buckets vs. summary percentiles ([prometheus]).\n\n\
             Set [general] interval (default 1s) to match your Prometheus scrape interval:\n\
             shorter than the scrape leaves gaps between summary samples, longer serves stale\n\
             data. See config/exporter.toml for a documented starting point.\n\n\
             EXAMPLE:\n    \
             # Expose a local agent to Prometheus using the example config\n    \
             rezolus exporter config/exporter.toml",
        )
        .arg(
            clap::Arg::new("CONFIG")
                .help("Path to the exporter TOML config (e.g. config/exporter.toml); see that file for the [general]/[prometheus] keys")
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
    let config: Arc<Config> = config.into();

    let _log_drain = configure_logging(config.log().level().to_tracing_level());

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

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    let c = config.clone();
    rt.spawn(async move { serve(c).await });

    rt.block_on(async move {
        let mut interval = crate::common::aligned_interval(config.general().interval().into());

        let mut previous = None;

        let url = config.general().mpk_url();

        loop {
            interval.tick().await;

            let start = Instant::now();

            // This loop ticks once per configured scrape interval (default
            // 1s, meant to track the Prometheus scrape cadence a few
            // seconds or more) — slow enough that logging every failure at
            // `error!` is the right cadence here, not a warn-once: an
            // operator watching this needs to see exactly how long an
            // outage lasts, and a single easily-scrolled-past line would
            // undersell an ongoing blackout. Before this, all three steps
            // below failed completely silently: against a v3 agent, a
            // pre-V3 exporter build served HTTP 200 with an empty body on
            // `/metrics` forever, with nothing in the logs at any level to
            // say why — an undiagnosable fleet-wide blackout.
            match client.get(url.clone()).send().await {
                Ok(response) => match response.bytes().await {
                    Ok(body) => {
                        let latency = start.elapsed();

                        debug!("sampling latency: {} us", latency.as_micros());

                        let mut reader = std::io::Cursor::new(body.as_ref());

                        match rmp_serde::from_read::<&mut std::io::Cursor<&[u8]>, Snapshot>(
                            &mut reader,
                        ) {
                            Ok(current) => {
                                if let Some(previous) = previous.take() {
                                    let snapshot =
                                        snapshot(&config, previous, current.clone(), latency);

                                    let mut s = SNAPSHOT.lock();
                                    *s = Some(snapshot);
                                }

                                previous = Some(current);
                            }
                            Err(e) => {
                                error!(
                                    "failed to decode snapshot from {url}: {e} — agent may be \
                                     emitting a newer snapshot format than this exporter \
                                     understands (e.g. snapshot_format = \"v3\" on the agent \
                                     against an exporter built before SnapshotV3 existed); \
                                     rebuild the exporter against a compatible \
                                     metriken-exposition version, or set snapshot_format = \
                                     \"v2\" on the agent"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!("failed to read response body from {url}: {e}");
                    }
                },
                Err(e) => {
                    error!("failed to reach agent at {url}: {e}");
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

/// A valid msgpack document carrying no metrics.
///
/// Served whenever the agent's body cannot be obtained. The obvious thing to
/// return is no bytes at all, and for a scraper that only ever reads the body
/// that is indistinguishable from this. It is not indistinguishable to a
/// consumer that *probes* the endpoint to decide what protocol it speaks: a
/// zero-byte body is not a msgpack document, so the probe concludes this is
/// not a msgpack endpoint and falls through to the Prometheus route this
/// exporter also serves.
///
/// `rezolus record` probes exactly that way. Pointed at an exporter whose
/// source was not up yet, it reported `detected Prometheus`, refused with
/// `.rez requires a rezolus (msgpack) endpoint`, and wrote nothing -- for the
/// entire run, even though the source came up seconds later and the exporter
/// served real snapshots from then on. One unavailable moment at startup cost
/// the whole recording, and the failure named the wrong cause.
///
/// An empty snapshot says "I am a msgpack endpoint and I have nothing for you
/// yet", which is both true and recoverable: the consumer keeps its
/// connection, decodes empty samples, and picks up real data the moment the
/// agent appears.
fn empty_snapshot() -> Vec<u8> {
    let snapshot = Snapshot::V2(SnapshotV2 {
        systemtime: SystemTime::now(),
        duration: Duration::ZERO,
        metadata: HashMap::new(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    });

    // Encoding a struct of empty vecs cannot fail, but an exporter that is
    // already coping with an unreachable agent must not panic on the fallback
    // path -- serving nothing is worse than serving an empty document, but it
    // beats taking the process down.
    rmp_serde::encode::to_vec(&snapshot).unwrap_or_default()
}

// for convenience, this proxies the msgpack from Rezolus Agent
async fn msgpack(State(state): State<Arc<AppState>>) -> Vec<u8> {
    if let Ok(response) = state.client.get(state.mpk_url.clone()).send().await {
        if let Ok(body) = response.bytes().await {
            // An empty body from the agent is passed on as an empty snapshot
            // for the same reason: it is not a msgpack document either.
            if !body.is_empty() {
                return body.to_vec();
            }
        }
    }

    empty_snapshot()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression: the fallback body must be a msgpack document, not
    /// nothing. A consumer probing this endpoint decides what protocol it
    /// speaks from these bytes, and zero of them reads as "not msgpack" --
    /// which sent `rezolus record` to the Prometheus route and cost it the
    /// whole recording.
    #[test]
    fn the_unavailable_body_is_still_msgpack() {
        let body = empty_snapshot();

        assert!(
            !body.is_empty(),
            "an unreachable agent must still yield a msgpack document"
        );

        let decoded: Snapshot = rmp_serde::from_slice(&body)
            .expect("the fallback body must decode as a snapshot, not merely be non-empty");

        match decoded {
            Snapshot::V2(s) => {
                // Empty, not fabricated: saying "nothing yet" is recoverable,
                // inventing a sample is not.
                assert!(s.counters.is_empty(), "must not invent counters");
                assert!(s.gauges.is_empty(), "must not invent gauges");
                assert!(s.histograms.is_empty(), "must not invent histograms");
            }
            other => panic!("expected a V2 snapshot, got {other:?}"),
        }
    }
}
