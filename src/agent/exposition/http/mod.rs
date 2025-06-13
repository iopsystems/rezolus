use crate::agent::*;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

mod snapshot;

use snapshot::SnapshotBuilder;

pub async fn serve(config: Arc<Config>, samplers: Arc<Box<[Box<dyn Sampler>]>>) {
    let state = Arc::new(Mutex::new(SnapshotBuilder::new(config.clone(), samplers)));

    let app: Router = app(state);

    let listener = TcpListener::bind(config.general().listen())
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

fn app(state: Arc<Mutex<SnapshotBuilder>>) -> Router {
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

async fn msgpack(State(state): State<Arc<Mutex<SnapshotBuilder>>>) -> Vec<u8> {
    let mut snapshot_builder = state.lock().await;
    let snapshot = snapshot_builder.build().await;
    rmp_serde::encode::to_vec(&snapshot).expect("failed to serialize snapshot")
}

async fn json(State(state): State<Arc<Mutex<SnapshotBuilder>>>) -> String {
    let mut snapshot_builder = state.lock().await;
    let snapshot = snapshot_builder.build().await;
    serde_json::to_string(&snapshot).expect("failed to serialize snapshot")
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Agent\nFor information, see: https://rezolus.com\n")
}
