use crate::agent::clock::SampleClock;
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

pub use snapshot::SnapshotBuilder;

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

    // The clock is created unconditionally but does NOT sample unconditionally
    // — with no subscriber it parks and arms no timer. See `agent::clock` for
    // why an agent nobody is watching must not sample.
    let clock = SampleClock::new(config.general().min_sample_interval());
    tokio::spawn(clock.clone().run(state.clone()));

    let app: Router = app(AppState {
        builder: state,
        clock,
    });

    let listener = TcpListener::bind(config.general().listen())
        .await
        .expect("failed to listen");

    axum::serve(listener, app)
        .await
        .expect("failed to run http server");
}

/// What every handler needs: the snapshot builder, and the clock a streaming
/// subscriber registers its demand with.
#[derive(Clone)]
pub(crate) struct AppState {
    builder: Arc<Mutex<SnapshotBuilder>>,
    clock: SampleClock,
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/metrics/binary", get(msgpack))
        .route("/metrics/rows", get(rows))
        .route("/metrics/stream", get(stream))
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

async fn msgpack(State(state): State<AppState>) -> bytes::Bytes {
    let now = Instant::now();

    let mut snapshot_builder = state.builder.lock().await;
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
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<RowsQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let now = Instant::now();
    let all = query.schemas.as_deref() == Some("all");

    let mut snapshot_builder = state.builder.lock().await;
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

/// Query parameters for [`stream`].
#[derive(serde::Deserialize, Default)]
struct StreamQuery {
    /// Requested sampling interval, e.g. `500ms`. Clamped to the agent's
    /// `min_sample_interval` floor, and reported back in frame 0 — a
    /// subscriber is told what it will actually get rather than assuming it
    /// got what it asked for.
    interval: Option<String>,
}

/// Subscribe to this agent: a stream of row frames, one per sampling tick.
///
/// # Why this is not a repeated `/metrics/rows`
///
/// `/metrics/rows` cannot know what any given consumer holds — its
/// `emitted_schemas` records what the AGENT has sent, so a consumer arriving
/// late, or one whose response was dropped, can be handed a reference to a
/// schema it never received. `?schemas=all` exists to dig such a consumer out.
///
/// A connection has none of that ambiguity. What this socket has been sent is
/// exactly knowable, so schema state lives here, per connection, and the
/// recovery hatch becomes simply the first frame.
///
/// # Frames
///
/// Each frame is a `u32` big-endian length followed by that many bytes of
/// msgpack [`AgentRows`](crate::recorder::wire::AgentRows). The first frame
/// carries every schema; later frames carry a schema only where it changed
/// for THIS connection.
///
/// # Backpressure
///
/// The agent is always-on production and must never be held up by a consumer
/// that has stopped reading — a recorder with a full disk must degrade to a
/// dropped subscription, never to a stalled agent. The sampling clock is
/// therefore never awaited by this handler: it watches a generation counter
/// and, if it finds itself more than one generation behind while writing, it
/// ends the stream rather than trying to catch up. A truncated stream is a
/// visible, recoverable event; a stalled agent is not.
async fn stream(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<StreamQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let requested = match query.interval.as_deref() {
        Some(s) => match s.parse::<humantime::Duration>() {
            Ok(d) => *d,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("bad interval {s:?}: {e}"),
                )
                    .into_response()
            }
        },
        None => Duration::from_secs(1),
    };

    // Refuse up front rather than accepting a subscription this agent can
    // never satisfy — see `SnapshotBuilder::serves_rows`. 409 matches
    // `/metrics/rows`: nothing is wrong, this agent just cannot answer this
    // question in its current configuration.
    if !state.builder.lock().await.serves_rows() {
        return (
            axum::http::StatusCode::CONFLICT,
            "the row format carries acquisition groups, which only a V3 snapshot has",
        )
            .into_response();
    }

    // Registering demand is what starts the clock; dropping the subscription
    // inside the stream's async block is what stops it.
    let subscription = state.clock.subscribe(requested);
    // What the subscriber will ACTUALLY get, which is not necessarily what it
    // asked for: the floor clamps it, and another subscriber may already be
    // driving the clock faster. Reported rather than left to be inferred — a
    // recorder that assumed its request was honoured would stamp its recording
    // with an interval the agent never used.
    let granted = subscription.interval();
    let builder = state.builder.clone();

    let body = axum::body::Body::from_stream(rows_frames(builder, subscription));
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static(crate::recorder::wire::STREAM_CONTENT_TYPE),
            ),
            (
                axum::http::HeaderName::from_static("x-rezolus-sample-interval"),
                axum::http::HeaderValue::from_str(&format!(
                    "{}",
                    humantime::format_duration(granted)
                ))
                .unwrap_or(axum::http::HeaderValue::from_static("unknown")),
            ),
        ],
        body,
    )
        .into_response()
}

/// The frame stream behind [`stream`]. Holds the [`Subscription`] for its
/// whole life, so the agent returns to tickless when the client goes away.
fn rows_frames(
    builder: Arc<Mutex<SnapshotBuilder>>,
    subscription: crate::agent::clock::Subscription,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    async_stream::try_stream! {
        let mut generation = subscription.generation();
        // Schemas already sent ON THIS CONNECTION — the per-connection state
        // that makes `?schemas=all` unnecessary here.
        let mut sent: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();

        loop {
            // Wait for a tick. `changed()` resolves IMMEDIATELY when the clock
            // moved on while the previous frame was being written — and it
            // yields the latest value, not the next unseen one, so a consumer
            // that fell behind is served the newest snapshot and the ticks in
            // between are skipped.
            //
            // That is the right behaviour (stale data helps nobody) but it is
            // why the frame's `seq` is the CLOCK GENERATION rather than a
            // count of frames sent. A per-frame counter would increment by one
            // across a skip, so the consumer would see a contiguous sequence
            // with data missing from the middle of it — a hole in a recording
            // that nothing could detect. As the generation, a skip is a
            // visible jump.
            if generation.changed().await.is_err() {
                break;
            }
            let observed = *generation.borrow_and_update();

            let rows = {
                let builder = builder.lock().await;
                match builder.latest_rows() {
                    Some(rows) => rows,
                    // No snapshot yet — the clock has not completed a pass.
                    None => continue,
                }
            };

            // The tick is shared; which schemas THIS connection still needs is
            // not. `encode_frame_filtered` applies that decision while
            // borrowing, so a second subscriber costs a serialization rather
            // than a copy of every payload.
            let frame = crate::recorder::wire::encode_frame_filtered(&rows, observed, |row| {
                match sent.get(&row.stream) {
                    Some(hash) if *hash == row.schema_hash => false,
                    _ => {
                        sent.insert(row.stream.clone(), row.schema_hash);
                        true
                    }
                }
            })
            .map_err(std::io::Error::other)?;

            yield bytes::Bytes::from(frame);

            // Lag check AFTER the write: if the clock moved on while we were
            // writing, this consumer cannot keep up. End the stream so it
            // reconnects (and, once backfill exists, asks for the gap) rather
            // than falling further behind or applying backpressure.
            if *generation.borrow() > observed + 1 {
                Err(std::io::Error::other(
                    "subscriber fell behind the sampling clock",
                ))?;
            }
        }
    }
}

async fn json(State(state): State<AppState>) -> String {
    let now = Instant::now();

    let mut snapshot_builder = state.builder.lock().await;
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

async fn status(
    State(state): State<AppState>,
) -> axum::response::Json<crate::agent::sampler_status::AgentStatus> {
    axum::response::Json(crate::agent::sampler_status::AgentStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        producer_epoch: crate::agent::epoch::producer_epoch().to_string(),
        uptime_seconds: crate::agent::agent_uptime_seconds(),
        ttl_seconds: STATUS_TTL_SECONDS.get().copied().unwrap_or(0),
        sample_interval_ms: state.clock.current().map(|d| d.as_millis() as u64),
        subscribers: state.clock.subscribers(),
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
