use crate::agent::clock::{self, Subscribers};
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

    // No clock is started here. Each subscription drives its own timer, so an
    // agent nobody is subscribed to samples only when scraped — see
    // `agent::clock` for why that matters beyond CPU.
    let app: Router = app(AppState {
        builder: state,
        subscribers: Subscribers::new(),
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
    subscribers: Subscribers,
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
    /// Sampling interval, e.g. `500ms`, defaulting to 1s. Honoured exactly:
    /// this subscription gets its own timer on its own boundaries, so the
    /// interval need not relate to any other subscriber's.
    ///
    /// What it does NOT control is how often the agent samples. A tick inside
    /// the snapshot TTL is answered from cache — and a reading already sent is
    /// **not sent again**, it is skipped. So asking for less than the TTL
    /// makes frames arrive at the TTL's rate, with the `seq` gaps saying which
    /// intervals produced nothing new. The TTL is the floor, and it belongs to
    /// the operator rather than to the subscriber.
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
            Ok(d) if d.is_zero() => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    "interval must be greater than zero",
                )
                    .into_response()
            }
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

    let subscription = state.subscribers.register(requested);
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
                    humantime::format_duration(requested)
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
///
/// [`Subscription`]: crate::agent::clock::Subscription
fn rows_frames(
    builder: Arc<Mutex<SnapshotBuilder>>,
    subscription: crate::agent::clock::Subscription,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    async_stream::try_stream! {
        let interval = subscription.interval();
        // Schemas already sent ON THIS CONNECTION — the per-connection state
        // that makes `/metrics/rows`'s `?schemas=all` recovery unnecessary
        // here.
        let mut sent: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        // Per group, the end of the acquisition window this connection has
        // already been told about. A snapshot carries every group the agent
        // knows, including ones whose sampler did not read this tick — a 60s
        // `drivehealth` sweep sits unchanged across hundreds of ticks, keeping
        // the window of its last real read. Sending it again would assert an
        // observation that did not happen.
        let mut last_window: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut last_sent_wall: Option<u64> = None;
        let mut last_index: Option<u64> = None;

        loop {
            // This subscription's own timer, on ITS boundaries. Nothing is
            // shared with other subscriptions: they neither speed this one up
            // nor slow it down, and an interval that divides nothing else is
            // served exactly rather than quantized.
            tokio::time::sleep(clock::until_next_aligned(interval)).await;

            // Which of this subscription's intervals just elapsed. Taken from
            // the boundary we woke for, not from the snapshot's own timestamp:
            // a snapshot served from cache can predate the boundary slightly,
            // and this index has to advance once per interval regardless.
            let index = clock::interval_index(wall_now_ns(), interval);

            // Ask for a snapshot. Whether this costs a sampling pass is the
            // TTL's decision, made in `rows_at` — which is what keeps a
            // subscription from sampling faster than the operator allowed, and
            // what lets two subscriptions whose ticks nearly coincide share one
            // pass.
            let rows = {
                let mut builder = builder.lock().await;
                match builder.rows_at(Instant::now()).await {
                    Some(rows) => rows,
                    None => continue,
                }
            };

            // Already sent this reading: skip rather than repeat it. Reached
            // when the interval asked for is shorter than the TTL, which is
            // the case the TTL exists to bound.
            //
            // Skipping rather than re-sending because a duplicate frame is
            // waste at both ends — bytes on the wire, and a decode on a
            // recorder that would discard it anyway (`stage_rows` drops a
            // group whose acquisition window has not advanced). The `seq` gap
            // left behind carries the information the duplicate would have:
            // this interval produced nothing new.
            //
            // The consequence, worth knowing: a subscription asking for much
            // less than the TTL sees SILENCE between frames rather than
            // duplicates, and this stream has no keepalive. Harmless at the
            // defaults (a 10ms TTL against intervals of a second or more, so
            // every tick brings a new reading) but it would bite an operator
            // who raised the TTL far above a subscriber's interval.
            if last_sent_wall == Some(rows.wall_ns) {
                continue;
            }
            last_sent_wall = Some(rows.wall_ns);

            if let Some(previous) = last_index {
                if index > previous + 1 {
                    warn!(
                        "stream subscriber at {interval:?} produced no frame for {} interval(s)",
                        index - previous - 1
                    );
                }
            }
            last_index = Some(index);

            // The snapshot may be shared with other subscriptions; which
            // schemas THIS connection still needs is not.
            // `encode_frame_filtered` applies that decision while borrowing,
            // so a second subscriber costs a serialization rather than a copy
            // of every payload.
            let frame = crate::recorder::wire::encode_frame_filtered(&rows, index, |row| {
                use crate::recorder::wire::RowDisposition;

                // Has this group actually been read again since this
                // connection last heard about it? A windowless group carries
                // no answer, so it is always sent — the same disposition
                // `stage_rows` gives it.
                if let Some(end) = row.window.map(|(_, end)| end) {
                    if last_window.get(&row.stream) == Some(&end) {
                        return RowDisposition::Omit;
                    }
                    last_window.insert(row.stream.clone(), end);
                }

                // Only now decide about the schema. Doing it the other way
                // round would record a schema as taught on a row that was
                // then omitted, and the group would go on to reference a
                // generation this consumer never received.
                match sent.get(&row.stream) {
                    Some(hash) if *hash == row.schema_hash => RowDisposition::Send,
                    _ => {
                        sent.insert(row.stream.clone(), row.schema_hash);
                        RowDisposition::SendWithSchema
                    }
                }
            })
            .map_err(std::io::Error::other)?;

            // Every group was already current for this consumer. That is not
            // the same as a tick on which nothing was observed, so it gets no
            // frame — the `seq` gap says an interval passed with no update.
            let Some(frame) = frame else {
                continue;
            };

            yield bytes::Bytes::from(frame);
        }
    }
}

/// Wall clock in nanoseconds since the Unix epoch.
fn wall_now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
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
        sample_interval_ms: state.subscribers.fastest().map(|d| d.as_millis() as u64),
        subscribers: state.subscribers.count(),
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
