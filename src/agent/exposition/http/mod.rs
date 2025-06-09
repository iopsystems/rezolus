use crate::agent::*;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use metriken_exposition::Snapshot;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

use std::time::{Instant, SystemTime};

mod snapshot;

struct AppState {
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
}

impl AppState {
    async fn refresh(&self) {
        let s: Vec<_> = self
            .samplers
            .iter()
            .map(|s| s.refresh_with_logging())
            .collect();

        let start = Instant::now();
        futures::future::join_all(s).await;
        let duration = start.elapsed().as_micros();
        debug!("sampling latency: {duration} us");
    }
}

pub async fn serve(config: Arc<Config>, samplers: Arc<Box<[Box<dyn Sampler>]>>) {
    let state = Arc::new(AppState { samplers });

    let app: Router = app(state);

    let listener = TcpListener::bind(config.general().listen())
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/metrics/binary", get(msgpack))
        .route("/metrics/json", get(json))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn take_snapshot(state: Arc<AppState>) -> Snapshot {
    let timestamp = SystemTime::now();
    let start = Instant::now();

    state.refresh().await;

    snapshot::create(timestamp, start.elapsed())
}

async fn msgpack(State(state): State<Arc<AppState>>) -> Vec<u8> {
    let snapshot = take_snapshot(state).await;
    rmp_serde::encode::to_vec(&snapshot).expect("failed to serialize snapshot")
}

async fn json(State(state): State<Arc<AppState>>) -> String {
    let snapshot = take_snapshot(state).await;
    serde_json::to_string(&snapshot).expect("failed to serialize snapshot")
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Agent\nFor information, see: https://rezolus.com\n")
}
