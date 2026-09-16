use crate::agent::*;

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use std::sync::OnceLock;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, decompression::RequestDecompressionLayer};

/// Snapshot-cache TTL in seconds, captured from config in `serve()`. Read by
/// the `/status` handler.
static STATUS_TTL_SECONDS: OnceLock<u64> = OnceLock::new();

mod snapshot;

use snapshot::SnapshotBuilder;

pub async fn serve(
    config: Arc<Config>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
    external_store: Option<Arc<ExternalMetricsStore>>,
) {
    let state = Arc::new(Mutex::new(SnapshotBuilder::new(
        config.clone(),
        samplers,
        external_store,
    )));

    let _ = STATUS_TTL_SECONDS.set(config.general().ttl().as_secs());

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
        .route("/metrics/rows", get(rows))
        .route("/metrics/json", get(json))
        .route("/metrics/descriptions", get(descriptions))
        .route("/systeminfo", get(system_info))
        .route("/samplers", get(samplers))
        .route("/status", get(status))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(RequestDecompressionLayer::new())
                .layer(CompressionLayer::new()),
        )
}

async fn msgpack(State(state): State<Arc<Mutex<SnapshotBuilder>>>) -> bytes::Bytes {
    let now = Instant::now();

    let mut snapshot_builder = state.lock().await;
    snapshot_builder.build_msgpack(now).await
}

/// Query parameters for [`rows`].
#[derive(serde::Deserialize, Default)]
struct RowsQuery {
    /// `schemas=all` forces every group's schema into the body. Any other
    /// value, or none, gives the delta body — schemas only where they changed.
    ///
    /// A recorder asks for `all` on its first scrape and whenever it meets a
    /// schema hash it cannot resolve; see `SnapshotBuilder::build_rows`.
    schemas: Option<String>,
}

/// Acquisition groups as pre-encoded WAL rows, for a consumer that keeps
/// state across scrapes — see `rez::wire`.
///
/// This is deliberately NOT a second spelling of `/metrics/binary`. Its body
/// omits schemas that have not changed, which a snapshot body can never do:
/// snapshots are contractually self-contained, because `record --format raw`
/// writes them verbatim for `recording convert` to decode offline.
async fn rows(
    State(state): State<Arc<Mutex<SnapshotBuilder>>>,
    axum::extract::Query(query): axum::extract::Query<RowsQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let now = Instant::now();
    let all = query.schemas.as_deref() == Some("all");

    let mut snapshot_builder = state.lock().await;
    match snapshot_builder.build_rows(now, all).await {
        Ok(body) => (
            [(
                axum::http::header::CONTENT_TYPE,
                crate::recorder::wire::CONTENT_TYPE,
            )],
            body,
        )
            .into_response(),
        // The only way this fails is an agent configured to produce V2
        // snapshots, which have no acquisition groups to serve. 409 rather
        // than 500: nothing went wrong, this agent just cannot answer this
        // question in its current configuration, and the recorder should fall
        // back to `/metrics/binary` rather than retry.
        Err(e) => (axum::http::StatusCode::CONFLICT, e).into_response(),
    }
}

async fn json(State(state): State<Arc<Mutex<SnapshotBuilder>>>) -> String {
    let now = Instant::now();

    let mut snapshot_builder = state.lock().await;
    snapshot_builder.build_json(now).await.to_string()
}

async fn root() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!("Rezolus {version} Agent\nFor information, see: https://rezolus.com\n")
}

async fn descriptions() -> axum::response::Json<std::collections::HashMap<String, String>> {
    let mut result = std::collections::HashMap::new();
    for metric in metriken::metrics().iter() {
        if let Some(description) = metric.description() {
            result
                .entry(metric.name().to_string())
                .or_insert_with(|| description.to_string());
        }
    }
    axum::response::Json(result)
}

async fn samplers() -> axum::response::Json<Vec<crate::agent::sampler_status::SamplerStatus>> {
    axum::response::Json(crate::agent::sampler_status::snapshot())
}

async fn status() -> axum::response::Json<crate::agent::sampler_status::AgentStatus> {
    axum::response::Json(crate::agent::sampler_status::AgentStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        producer_epoch: crate::agent::epoch::producer_epoch().to_string(),
        uptime_seconds: crate::agent::agent_uptime_seconds(),
        ttl_seconds: STATUS_TTL_SECONDS.get().copied().unwrap_or(0),
        samplers: crate::agent::sampler_status::snapshot(),
    })
}

async fn system_info() -> axum::response::Response {
    use axum::response::IntoResponse;

    match systeminfo::summary() {
        Some(info) => axum::response::Json(info).into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}
