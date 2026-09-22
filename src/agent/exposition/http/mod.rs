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

/// A sampling pass as dendro replication frames — #1224 Phase 2.
///
/// Nothing serves these yet: the `/metrics/stream` wiring is the next step.
/// Building the producer first keeps that change to the handler, against
/// frames whose round trip into an archive is already tested.
#[cfg_attr(not(test), allow(dead_code))]
mod frames;
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
        ttl: config.general().ttl(),
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
    /// The snapshot TTL, reported to a stream subscriber as the floor on how
    /// often its frames can carry anything new.
    ttl: Duration,
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
    /// Frame interval, e.g. `500ms`, defaulting to 1s. Honoured exactly: this
    /// subscription gets its own timer on its own boundaries, so the interval
    /// need not relate to any other subscriber's.
    ///
    /// The response reports it back as `x-rezolus-frame-interval`, alongside
    /// `x-rezolus-update-floor` — the snapshot TTL, which is how often a frame
    /// can carry anything new.
    ///
    /// What it does NOT control is how often the agent samples. A tick inside
    /// the snapshot TTL is answered from cache, and readings already sent are
    /// not repeated — that interval gets an EMPTY frame instead. So asking for
    /// less than the TTL yields frames at the requested rate carrying updates
    /// at the TTL's rate. The TTL is the floor, and it belongs to the operator
    /// rather than to the subscriber.
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
    let ttl = state.ttl;

    let body = axum::body::Body::from_stream(rows_frames(builder, subscription, wall_now_ns));
    let duration_header = |d: Duration| {
        axum::http::HeaderValue::from_str(&format!("{}", humantime::format_duration(d)))
            .unwrap_or(axum::http::HeaderValue::from_static("unknown"))
    };

    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static(frames::CONTENT_TYPE),
            ),
            // How often a frame arrives: exactly what was asked for, since
            // this subscription gets its own timer.
            (
                axum::http::HeaderName::from_static("x-rezolus-frame-interval"),
                duration_header(requested),
            ),
            // How often a frame can carry anything NEW, which is the thing a
            // subscriber cannot otherwise discover. Asking for less than this
            // is legal and gets the frames it asked for — most of them empty.
            // Reported so that a consumer stamping a recording with "1s data"
            // can tell when it is really getting 10s data.
            (
                axum::http::HeaderName::from_static("x-rezolus-update-floor"),
                duration_header(ttl),
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
///
/// `wall_now` is injected rather than read directly so a test can drive the
/// clock backwards and exercise the monotonicity clamp below on the REAL code
/// path. `tokio::time::pause` cannot do that job: it controls `Instant` and
/// `sleep`, while the interval index is derived from `SystemTime`, which tokio
/// does not touch.
fn rows_frames(
    builder: Arc<Mutex<SnapshotBuilder>>,
    subscription: crate::agent::clock::Subscription,
    wall_now: impl Fn() -> u64,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    async_stream::try_stream! {
        // Held for the life of this subscription. Identity is published and
        // folded into the index only while something wants it; without this the
        // agent maintains one for nobody, which measured at ~0.3 ms a scrape on
        // a host with busy task churn.
        let _identity = crate::agent::identity::Demand::register();
        let interval = subscription.interval();
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
        // The index state this connection has been brought to. `None` until
        // the opening `Full`, which is what rule 3 requires: a subscriber
        // starts complete or not at all.
        let mut last_state: Option<crate::recorder::index::IndexState> = None;

        let mut producer = frames::FrameProducer::new(
            crate::agent::epoch::producer_epoch().to_string(),
            [("source".to_string(), env!("CARGO_BIN_NAME").to_string())]
                .into_iter()
                .collect(),
            [(
                dendro::keys::PRODUCER_EPOCH.to_string(),
                crate::agent::epoch::producer_epoch().to_string(),
            )]
            .into_iter()
            .collect(),
        );

        // dendro's own framing: a preamble, then length-prefixed frames. Sent
        // before anything else so a consumer can reject a stream it cannot
        // read rather than decoding its first frame as garbage.
        let mut opening = Vec::new();
        dendro::replicate::wire::write_preamble(&mut opening)
            .map_err(std::io::Error::other)?;
        dendro::replicate::wire::encode_frame(&producer.handshake(), &mut opening)
            .map_err(std::io::Error::other)?;
        yield bytes::Bytes::from(opening);

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
            //
            // Clamped forward, because it is derived from the WALL clock and
            // the wall clock can step. An NTP correction backwards would
            // otherwise emit a `seq` lower than one already sent, which a
            // consumer checking for +1 has no rule for — it would read as
            // corruption.
            let measured = clock::interval_index(wall_now(), interval);
            let index = clock::monotonic_interval_index(wall_now(), interval, last_index);
            if index != measured {
                warn!(
                    "wall clock moved backwards under a stream subscriber at {interval:?}; \
                     holding the interval index monotonic ({measured} -> {index})"
                );
            }

            // Ask for a snapshot, then take the index in the SAME lock. Two
            // locks would let a sampling pass land between them, and the rows
            // would name a state built from a walk they were not part of.
            let (rows, entries, state) = {
                let mut builder = builder.lock().await;
                let Some(rows) = builder.rows_at(Instant::now()).await else {
                    continue;
                };
                let state = builder.index_state();
                // What this connection needs to reach `state`. `None` means it
                // fell out of the history — see `IndexHistory` — and only the
                // whole set will do.
                let entries = match last_state {
                    Some(held) => builder.index_since(held).unwrap_or_else(|| {
                        warn!(
                            "stream subscriber at {interval:?} fell out of the index history; \
                             resending the whole slot set"
                        );
                        builder.index_full()
                    }),
                    None => builder.index_full(),
                };
                (rows, entries, state)
            };
            last_state = Some(state);

            // The same reading as last time — reached when the interval asked
            // for is shorter than the TTL, which is the case the TTL exists to
            // bound. Nothing in this snapshot can have advanced, so every row
            // is dropped below and the frame goes out empty, saying "your
            // interval elapsed and there is nothing new".
            let advanced = last_sent_wall != Some(rows.wall_ns);
            last_sent_wall = Some(rows.wall_ns);

            if let Some(previous) = last_index {
                if index > previous + 1 {
                    // Every interval gets a frame, so this is a lost reading
                    // rather than a quiet one — the distinction empty frames
                    // exist to preserve.
                    warn!(
                        "stream subscriber at {interval:?} missed {} interval(s); \
                         the subscription was starved past its boundary",
                        index - previous - 1
                    );
                }
            }
            last_index = Some(index);

            let produced = producer.interval(
                &rows,
                entries,
                state,
                index,
                |row| {
                    // The whole snapshot is one this connection already has, so
                    // nothing in it is new — including a windowless group,
                    // which carries no evidence either way and would otherwise
                    // be sent again on the strength of not being able to prove
                    // itself stale.
                    if !advanced {
                        return false;
                    }
                    // Has this group actually been read again since this
                    // connection last heard about it? A windowless group
                    // carries no answer, so it is always sent — the same
                    // disposition `stage_rows` gives it.
                    if let Some(end) = row.window.map(|(_, end)| end) {
                        if last_window.get(&row.stream) == Some(&end) {
                            return false;
                        }
                        last_window.insert(row.stream.clone(), end);
                    }
                    true
                },
            );

            let mut body = Vec::new();
            for frame in &produced {
                dendro::replicate::wire::encode_frame(frame, &mut body)
                    .map_err(std::io::Error::other)?;
            }
            yield bytes::Bytes::from(body);
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
        clock_anchor_wall_ns: crate::agent::epoch::clock_anchor_wall_ns(),
        uptime_seconds: crate::agent::agent_uptime_seconds(),
        ttl_seconds: STATUS_TTL_SECONDS.get().copied().unwrap_or(0),
        sample_interval_ms: state.subscribers.fastest().map(|d| d.as_millis() as u64),
        subscribers: state.subscribers.count(),
        index_resyncs: state.builder.lock().await.index_resyncs(),
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

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::agent::clock::Subscribers;
    use crate::agent::config::Config;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The whole path, over a real socket: the agent serves, the recorder's
    /// own `Subscription` connects, and one interval comes back applied.
    ///
    /// Every other test on either side hands frames straight to the other's
    /// types. This one goes through axum, HTTP, the chunked body and the
    /// decoder — so a content type, a route, a header or a framing mistake
    /// fails here rather than the first time a recorder is pointed at an
    /// agent.
    ///
    /// It also covers the empty-index case specifically: with no samplers the
    /// agent has no slots, so it sends no index frames at all and its rows name
    /// the empty state. A subscriber that required an index entry before
    /// attributing anything would skip every row of this stream.
    #[tokio::test]
    async fn a_recorder_can_subscribe_to_a_real_agent_over_http() {
        use crate::recorder::stream::Subscription;

        let config: Config = toml::from_str("[general]\nttl = \"1s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let state = AppState {
            builder: Arc::new(Mutex::new(SnapshotBuilder::new(
                Arc::new(config),
                Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
                None,
            ))),
            subscribers: Subscribers::new(),
            ttl: Duration::from_secs(1),
        };

        // Port 0: the OS picks a free one, so this cannot collide with another
        // test or with something already running on the machine.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app(state)).await;
        });

        // `main` installs this; a test binary never runs `main`. A second
        // install returns `Err` rather than panicking, so ignoring the result
        // is what makes this safe to call from any test that needs a client.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        let base = reqwest::Url::parse(&format!("http://{addr}")).unwrap();
        let mut sub = Subscription::connect(&client, &base, Duration::from_secs(1))
            .await
            .expect("subscribes");
        assert!(
            sub.source().is_some(),
            "connect returns with the handshake applied: the recorder opens the \
             recording on its anchor before the first interval"
        );

        let applied = tokio::time::timeout(Duration::from_secs(10), sub.next_interval())
            .await
            .expect("an interval arrives inside the timeout")
            .expect("the stream is well formed")
            .expect("the stream did not end");

        assert_eq!(
            applied.rows_skipped, 0,
            "an agent with no slots names the empty state, which is the state a fresh \
             subscriber holds"
        );
        assert_eq!(
            sub.source()
                .and_then(|s| s.labels.get("source"))
                .map(String::as_str),
            Some("rezolus"),
            "the handshake arrived and identified the source"
        );
        assert_eq!(sub.skipped_total(), 0);
    }

    /// Drive the real frame stream with a wall clock the test owns, and step
    /// it BACKWARDS mid-stream.
    ///
    /// This exercises the shipped code path rather than modelling it. An
    /// earlier version of this test re-implemented the clamp in its own body,
    /// so deleting the clamp from the handler left it passing — it proved the
    /// arithmetic and nothing about the stream.
    ///
    /// `tokio::time::pause` cannot do this job: it controls `Instant` and
    /// `sleep`, while the interval index is derived from `SystemTime`. Hence
    /// the injected clock.
    #[tokio::test(start_paused = true)]
    async fn a_backwards_clock_step_cannot_lower_seq_on_the_real_stream() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let builder = Arc::new(Mutex::new(SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        )));
        let subscription = Subscribers::new().register(Duration::from_secs(1));

        // A wall clock this test drives.
        let now = Arc::new(AtomicU64::new(1_700_000_000_000_000_000));
        let clock_for_stream = Arc::clone(&now);

        let stream = rows_frames(builder, subscription, move || {
            clock_for_stream.load(Ordering::Relaxed)
        });
        futures::pin_mut!(stream);

        // Every chunk concatenated, preamble included, then read back through
        // dendro's own reader — the bytes a subscriber would actually receive.
        let mut body = Vec::new();
        // The opening chunk is the preamble and the handshake, before any
        // interval has elapsed.
        body.extend_from_slice(
            &stream
                .next()
                .await
                .expect("the stream opens")
                .expect("an opening chunk"),
        );

        for step in 0..6 {
            // Three seconds forward, then a thirty-second jump BACKWARDS —
            // an NTP correction of the kind that would otherwise emit a `seq`
            // below one already sent.
            if step == 3 {
                now.fetch_sub(30_000_000_000, Ordering::Relaxed);
            } else {
                now.fetch_add(1_000_000_000, Ordering::Relaxed);
            }
            body.extend_from_slice(
                &stream
                    .next()
                    .await
                    .expect("the stream yields")
                    .expect("a frame"),
            );
        }

        let mut reader = dendro::replicate::wire::FrameReader::new(std::io::Cursor::new(body))
            .expect("the preamble reads");
        let mut seqs = Vec::new();
        let mut handshakes = 0usize;
        while let Some(frame) = reader.next_frame().expect("decodable") {
            match frame {
                dendro::replicate::Frame::Handshake { .. } => handshakes += 1,
                dendro::replicate::Frame::Rows { seq, .. } => seqs.push(seq),
                _ => {}
            }
        }
        assert_eq!(handshakes, 1, "one handshake, at the start and only there");
        assert_eq!(seqs.len(), 6, "one rows frame per interval");

        for pair in seqs.windows(2) {
            assert!(
                pair[1] > pair[0],
                "seq went backwards or stalled across a clock step: {seqs:?}"
            );
        }
        // ...and specifically: the step back did not produce a lower index.
        assert!(
            seqs[3] == seqs[2] + 1,
            "the clamped frame must continue the sequence, got {seqs:?}"
        );
    }

    fn test_state(config_toml: &str) -> AppState {
        let config: Config = toml::from_str(config_toml).expect("valid config");
        AppState {
            builder: Arc::new(Mutex::new(SnapshotBuilder::new(
                Arc::new(config),
                Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
                None,
            ))),
            subscribers: Subscribers::new(),
            ttl: Duration::from_secs(1),
        }
    }

    fn test_client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder().http1_only().build().unwrap()
    }

    /// Serve `router` on a free port from the test's own runtime.
    async fn serve(router: Router) -> reqwest::Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        reqwest::Url::parse(&format!("http://{addr}")).unwrap()
    }

    /// `--stream` against an agent that cannot serve it fails loudly, and
    /// the two ways an agent can fail to serve it both classify as
    /// `Unsupported`: retrying will not change either answer, so the recorder
    /// must not sit in its retry loop on them.
    #[tokio::test]
    async fn an_agent_that_cannot_serve_the_stream_is_refused_as_unsupported() {
        use crate::recorder::stream::{ConnectError, Subscription};
        let client = test_client();

        // An agent from before the route existed: 404.
        let old = Router::new().route("/", get(root));
        let base = serve(old).await;
        match Subscription::connect(&client, &base, Duration::from_secs(1)).await {
            Err(ConnectError::Unsupported(e)) => {
                assert!(e.contains("404"), "{e}");
                assert!(e.contains("/metrics/stream"), "names the route: {e}");
            }
            Err(ConnectError::Unreachable(e)) => panic!("a 404 is an answer, not an outage: {e}"),
            Ok(_) => panic!("there is no stream to subscribe to"),
        }

        // A V2 agent: the route exists and answers 409.
        let v2 = test_state("[general]\nttl = \"1s\"\nsnapshot_format = \"v2\"\n");
        let base = serve(app(v2)).await;
        match Subscription::connect(&client, &base, Duration::from_secs(1)).await {
            Err(ConnectError::Unsupported(e)) => {
                assert!(e.contains("V2"), "says what kind of agent this is: {e}");
            }
            Err(ConnectError::Unreachable(e)) => panic!("a 409 is an answer, not an outage: {e}"),
            Ok(_) => panic!("a V2 agent has nothing to stream"),
        }
    }

    /// Nothing listening is the other class: an outage, retried each tick
    /// exactly as a scrape of a down endpoint is.
    #[tokio::test]
    async fn a_port_with_nothing_on_it_is_unreachable() {
        use crate::recorder::stream::{ConnectError, Subscription};
        // Bind to learn a free port, then release it.
        let addr = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let base = reqwest::Url::parse(&format!("http://{addr}")).unwrap();
        match Subscription::connect(&test_client(), &base, Duration::from_secs(1)).await {
            Err(ConnectError::Unreachable(_)) => {}
            Err(ConnectError::Unsupported(e)) => {
                panic!("a refused connection is an outage, not a refusal: {e}")
            }
            Ok(_) => panic!("nothing is listening"),
        }
    }

    /// An agent on its own runtime on its own thread, so that stopping it
    /// closes every connection it holds — the shape a crashed or restarted
    /// agent has on the wire. Aborting an `axum::serve` task would not do:
    /// it spawns a task per connection, and those outlive the acceptor.
    struct KillableAgent {
        addr: std::net::SocketAddr,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl KillableAgent {
        /// Start on `addr`, or a free port when `None`. Binding retries for
        /// a few seconds so a restart on the port a previous agent just
        /// released does not race its close.
        fn start(addr: Option<std::net::SocketAddr>) -> Self {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let listener = loop {
                match std::net::TcpListener::bind(
                    addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
                ) {
                    Ok(l) => break l,
                    Err(e) if std::time::Instant::now() < deadline => {
                        let _ = e;
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => panic!("could not bind {addr:?}: {e}"),
                }
            };
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let thread = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let state = test_state("[general]\nttl = \"1s\"\nsnapshot_format = \"v3\"\n");
                    tokio::select! {
                        _ = axum::serve(listener, app(state)) => {}
                        _ = stopped => {}
                    }
                });
                // The runtime drops here, and every connection task with it.
            });
            Self {
                addr,
                stop: Some(stop),
                thread: Some(thread),
            }
        }

        fn kill(mut self) -> std::net::SocketAddr {
            let _ = self.stop.take().unwrap().send(());
            self.thread.take().unwrap().join().unwrap();
            self.addr
        }
    }

    impl Drop for KillableAgent {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// The pump's whole life: intervals flow, the agent goes away, the drop
    /// is reported, the agent comes back, the reconnect is reported with the
    /// source its new handshake named, and intervals flow again — through
    /// the real socket, the real producer and the real subscriber.
    ///
    /// The rows between the drop and the reconnect are lost, and the report
    /// is what records that: the reconnected subscription's own `gap` cannot,
    /// because it counts from its own first frame.
    #[tokio::test]
    async fn a_dropped_stream_is_reconnected_and_both_ends_are_reported() {
        use crate::recorder::stream::{pump, StreamEvent, Subscription};

        let agent = KillableAgent::start(None);
        let client = test_client();
        let base = reqwest::Url::parse(&format!("http://{}", agent.addr)).unwrap();
        let interval = Duration::from_secs(1);

        let sub = Subscription::connect(&client, &base, interval)
            .await
            .expect("subscribes");
        let first = sub.source().cloned().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(pump(3, sub, client.clone(), base.clone(), interval, tx));

        // Waits for the next event of the kind `want` accepts, skipping the
        // intervals that keep arriving in between.
        async fn next_matching<T>(
            rx: &mut tokio::sync::mpsc::Receiver<(usize, StreamEvent)>,
            mut want: impl FnMut(StreamEvent) -> Option<T>,
        ) -> T {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                let (idx, event) = tokio::time::timeout_at(deadline, rx.recv())
                    .await
                    .expect("an event arrives inside the deadline")
                    .expect("the pump is alive");
                assert_eq!(idx, 3, "events carry the endpoint they are for");
                if let Some(t) = want(event) {
                    return t;
                }
            }
        }

        let applied = next_matching(&mut rx, |e| match e {
            StreamEvent::Interval(a) => Some(a),
            _ => None,
        })
        .await;
        assert_eq!(applied.rows_skipped, 0);

        let addr = agent.kill();
        let reason = next_matching(&mut rx, |e| match e {
            StreamEvent::Dropped(reason) => Some(reason),
            StreamEvent::Interval(_) => None,
            other => panic!("before a reconnect there is nothing else to report: {other:?}"),
        })
        .await;
        assert!(!reason.is_empty());

        // Back on the same port. The pump retries every `interval` (floored
        // at a second), so this is found on its next attempt.
        let _agent = KillableAgent::start(Some(addr));
        let source = next_matching(&mut rx, |e| match e {
            StreamEvent::Connected(source) => Some(source),
            StreamEvent::Refused(e) => panic!("a v3 agent came back: {e}"),
            _ => None,
        })
        .await;
        assert_eq!(
            source.uuid, first.uuid,
            "same process, same epoch: the loop is told the source so it can tell"
        );

        // And it is flowing again.
        let again = next_matching(&mut rx, |e| match e {
            StreamEvent::Interval(a) => Some(a),
            _ => None,
        })
        .await;
        assert_eq!(again.rows_skipped, 0);
    }
}
