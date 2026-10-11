use crate::agent::clock::Subscribers;
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

/// Tests of the agent's rows through metriken-exposition's frame producer
/// and back through the recorder's subscriber.
#[cfg(test)]
mod frames;
mod router;
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

use metriken_exposition::stream::{Layout, StreamRequest, StreamSubscription};

/// Subscribe to this agent: a stream of row frames, one per interval.
///
/// The route is metriken-exposition's [`StreamSubscription`]: it parses the
/// request (`?interval=`, a humantime duration defaulting to 1s, and
/// `?layout=wide|long`), names the response's content type and headers
/// (`metriken_exposition::stream::WIRE_NAMES`), runs one timer per
/// subscription aligned to its interval since the Unix epoch, and decides
/// what each interval's frame carries. What stays here is how a pass is
/// fetched: the `SnapshotBuilder` lock, the sampler refresh behind the TTL,
/// and the pass cache `/metrics/rows` shares.
///
/// # Why this is not a repeated `/metrics/rows`
///
/// `/metrics/rows` cannot know what any given consumer holds — its
/// `emitted_schemas` records what the AGENT has sent, so a consumer arriving
/// late, or one whose response was dropped, can be handed a reference to a
/// schema it never received. `?schemas=all` exists to dig such a consumer out.
///
/// A connection has none of that ambiguity. What this socket has been sent is
/// exactly knowable, so schema state lives in the subscription, per
/// connection, and the first frame carries every schema.
///
/// # Interval and update floor
///
/// The interval is honoured exactly: each subscription has its own timer. It
/// does not control how often the agent samples. A tick inside the snapshot
/// TTL is answered from cache, and readings already sent are not repeated:
/// that interval gets an empty frame. The update-floor header reports the
/// TTL, so a subscriber asking for less can tell that its frames carry new
/// readings only at the TTL's rate.
///
/// # Layout
///
/// `?layout=long` sends a group whose metrics are all counter, gauge or
/// histogram groups in the long form: values keyed by occupant, and each
/// occupant's labels once, when it first appears to this connection. Without
/// the parameter, or with `layout=wide`, every group is a `WalGroupRow`. The
/// layout header names the layout served.
///
/// # A reader that stops reading
///
/// The body is a stream that hyper polls only when the connection can take
/// more bytes. A reader that stops reading leaves it suspended between
/// frames with no lock held: the `SnapshotBuilder` mutex is released once
/// the pass is fetched, so other subscribers and scrapes proceed. When the
/// reader resumes, the subscription sleeps to its next boundary and sends
/// the current pass rather than the ones it missed; the skipped intervals
/// show as a `seq` gap, which the subscription logs and the subscriber
/// sees. The stream is not ended for falling behind.
async fn stream(
    State(state): State<AppState>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let request = match StreamRequest::parse(query.as_deref().unwrap_or("")) {
        Ok(request) => request,
        Err(refusal) => {
            let status = axum::http::StatusCode::from_u16(refusal.status)
                .unwrap_or(axum::http::StatusCode::BAD_REQUEST);
            return (status, refusal.message).into_response();
        }
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

    // Held by the body for the stream's life, so `/status` counts this
    // subscriber until the client goes away.
    let registration = state.subscribers.register(request.interval);
    let subscription = StreamSubscription::new(
        request,
        state.ttl,
        [("source".to_string(), env!("CARGO_BIN_NAME").to_string())]
            .into_iter()
            .collect(),
        std::collections::BTreeMap::new(),
    );

    let mut headers = axum::http::HeaderMap::new();
    for (name, value) in subscription.headers() {
        if let Ok(value) = axum::http::HeaderValue::from_str(&value) {
            headers.insert(axum::http::HeaderName::from_static(name), value);
        }
    }
    let body = axum::body::Body::from_stream(subscription_body(
        subscription,
        state.builder.clone(),
        registration,
        std::time::SystemTime::now,
    ));
    (headers, body).into_response()
}

/// The bytes a stream subscription sends: its opening, then one frame per
/// interval, each from the pass `builder` gives for that interval.
///
/// `keep_alive` lives as long as the stream. `wall_now` is
/// `SystemTime::now` outside tests; a test injects its own to step the wall
/// clock, which `tokio::time::pause` does not control.
fn subscription_body(
    subscription: StreamSubscription,
    builder: Arc<Mutex<SnapshotBuilder>>,
    keep_alive: impl Send + 'static,
    wall_now: impl Fn() -> std::time::SystemTime + Send + 'static,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<bytes::Bytes>> + Send>> {
    // One `into_stream` per layout, since the two layouts' passes are
    // different types. The instant is read on waking, before the lock: a
    // subscriber queued behind another's pass then asks for the time it woke
    // at, and the pass built for that boundary is fresh for it.
    match subscription.request().layout {
        Layout::Wide => Box::pin(subscription.into_stream(
            move |_| {
                let builder = Arc::clone(&builder);
                let now = Instant::now();
                async move { builder.lock().await.rows_at(now).await }
            },
            keep_alive,
            wall_now,
        )),
        Layout::Long => Box::pin(subscription.into_stream(
            move |_| {
                let builder = Arc::clone(&builder);
                let now = Instant::now();
                async move { builder.lock().await.stream_long_at(now).await }
            },
            keep_alive,
            wall_now,
        )),
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
        clock_anchor_wall_ns: crate::agent::epoch::clock_anchor_wall_ns(),
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

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::agent::clock::Subscribers;
    use crate::agent::config::Config;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A one-second subscription in `layout`, as the handler builds one.
    fn subscription(layout: Layout) -> StreamSubscription {
        StreamSubscription::new(
            StreamRequest {
                interval: Duration::from_secs(1),
                layout,
            },
            Duration::from_secs(60),
            Default::default(),
            Default::default(),
        )
    }

    /// A wall clock the test drives, in nanoseconds since the Unix epoch.
    fn wall_clock(at: &Arc<AtomicU64>) -> impl Fn() -> std::time::SystemTime + Send + 'static {
        let at = Arc::clone(at);
        move || std::time::UNIX_EPOCH + Duration::from_nanos(at.load(Ordering::Relaxed))
    }

    /// The whole path, over a real socket: the agent serves, the recorder's
    /// own `Subscription` connects, and one interval comes back applied.
    ///
    /// Every other test on either side hands frames straight to the other's
    /// types. This one goes through axum, HTTP, the chunked body and the
    /// decoder — so a content type, a route, a header or a framing mistake
    /// fails here rather than the first time a recorder is pointed at an
    /// agent.
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

        // Build one snapshot before the clock below starts. The first pass
        // misses the skeleton cache and builds every group's schema from the
        // global registry: about 200 ms in a debug build alone, and seconds
        // when the rest of this binary's tests run beside it, which is how a
        // CI runner once spent the whole 10 s timeout before the first
        // interval. Later passes hit the cache, so the interval waited on
        // below is the stream's own latency rather than a one-time build.
        state.builder.lock().await.rows_at(Instant::now()).await;

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
        let mut sub = Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        .expect("subscribes");
        assert!(
            sub.source().is_some(),
            "connect returns with the handshake applied: the recorder opens the \
             recording on its anchor before the first interval"
        );

        tokio::time::timeout(Duration::from_secs(10), sub.next_interval())
            .await
            .expect("an interval arrives inside the timeout")
            .expect("the stream is well formed")
            .expect("the stream did not end");

        assert_eq!(
            sub.source()
                .and_then(|s| s.labels.get("source"))
                .map(String::as_str),
            Some("rezolus"),
            "the handshake arrived and identified the source"
        );
    }

    /// `?layout=long` over HTTP: the agent says it serves the long layout,
    /// a group of slots arrives as a long row after the occupants it
    /// introduces, and the second interval does not describe them again.
    #[tokio::test]
    async fn a_long_layout_subscription_sends_each_occupant_once() {
        use crate::recorder::stream::{Layout, Subscription};
        use metriken_storage::StreamedGroup;

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
        // Warm the builders, as the test above does, so the first interval is
        // the stream's own latency.
        state
            .builder
            .lock()
            .await
            .stream_long_at(Instant::now())
            .await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app(state)).await;
        });
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        let base = reqwest::Url::parse(&format!("http://{addr}")).unwrap();
        let mut sub = Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            Layout::Long,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        .expect("subscribes");
        assert_eq!(sub.layout(), Layout::Long);

        // Recorded as `record -o out.dendro` records it: decoded, then staged.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.dendro");
        let mut writer =
            metriken_storage::ArchiveWriter::create(&path, Default::default()).unwrap();
        let source = sub.source().cloned().unwrap();
        let mut recording = writer
            .add_source(
                source.labels.clone(),
                source.metadata.clone(),
                source.clock_anchor_wall_ns as u64,
            )
            .unwrap();

        // Per interval: occupants described, long rows, occupants present.
        let mut intervals: Vec<(usize, usize, usize)> = Vec::new();
        let mut last = (0u64, 0i64);
        let mut decoder = metriken_storage::StreamDecoder::new();
        while intervals.len() < 2 {
            let applied = tokio::time::timeout(Duration::from_secs(10), sub.next_interval())
                .await
                .expect("an interval arrives inside the timeout")
                .expect("the stream is well formed")
                .expect("the stream did not end");
            if applied.rows.is_empty() {
                continue;
            }
            let mut tally = (0, 0, 0);
            for pass in applied.for_writer().unwrap() {
                let groups = decoder.decode(pass.rows).unwrap();
                for g in &groups {
                    match g {
                        StreamedGroup::Occupants { occupants, .. } => tally.0 += occupants.len(),
                        StreamedGroup::Long { row, .. } => {
                            tally.1 += 1;
                            tally.2 += row.occupants.len();
                        }
                        StreamedGroup::Wide(_) => {}
                    }
                }
                last = (pass.ts as u64, pass.wall_offset);
                let staged = recording
                    .stage_streamed(groups, pass.ts as u64, pass.wall_offset)
                    .unwrap();
                writer.commit(vec![staged]).unwrap();
            }
            intervals.push(tally);
        }
        assert_eq!(decoder.unresolved, 0);
        let (described, long_rows, _) = intervals[0];
        assert!(long_rows > 0, "a group of slots arrives long");
        assert!(described > 0, "its occupants are described first");
        // Other tests in this binary change slot metadata in the shared
        // registry, so an occupant may change between the two intervals;
        // the ones that did not are not described again.
        let (described, _, present) = intervals[1];
        assert!(
            described < present,
            "{described} of {present} occupants described again"
        );

        recording.finalize(last).unwrap();
        writer.join().unwrap();
        let catalog = metriken_storage::DendroCatalog::open(&path).unwrap();
        use metriken_storage::Catalog;
        let id = catalog.sources().unwrap()[0].id;
        let tables = catalog.tables(id).unwrap();
        assert!(
            tables.iter().any(|t| t.ends_with("/occupants")),
            "the archive holds occupant streams: {tables:?}"
        );
        let reader = crate::rez_reader::RezReader::open_with_pool(
            &path,
            metriken_storage::BufferPool::new(16 << 20),
        )
        .expect("the recording opens");
        assert!(!metriken_query::MetricsSource::counter_names(&reader).is_empty());

        let bad = client
            .get(base.join("/metrics/stream?layout=diagonal").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    /// An interval with no reading to send still gets a frame: the empty one,
    /// with the next `seq`. A V2 builder has no acquisition groups, so its
    /// `rows_at` is `None` every interval, which is the case under test; the
    /// route refuses V2, so the stream is driven directly.
    ///
    /// Before this, such an interval was skipped: no frame, a `seq` gap a
    /// subscriber reads as lost intervals, and a stream that never yields at
    /// all while the reading stays missing. Each wait is bounded on the paused
    /// clock, so that failure shows as a timeout rather than a hung test.
    #[tokio::test(start_paused = true)]
    async fn an_interval_without_a_reading_still_gets_an_empty_frame() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\nsnapshot_format = \"v2\"\n")
            .expect("valid config");
        let builder = Arc::new(Mutex::new(SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        )));
        let registration = Subscribers::new().register(Duration::from_secs(1));
        let now = Arc::new(AtomicU64::new(1_700_000_000_000_000_000));
        let stream = subscription_body(
            subscription(Layout::Wide),
            builder,
            registration,
            wall_clock(&now),
        );
        futures::pin_mut!(stream);

        let mut body = Vec::new();
        for step in 0..5 {
            if step > 0 {
                now.fetch_add(1_000_000_000, Ordering::Relaxed);
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .unwrap_or_else(|_| panic!("chunk {step}: no frame within 10 s of stream time"))
                .expect("the stream yields")
                .expect("a chunk");
            body.extend_from_slice(&chunk);
        }

        let mut reader = dendro::replicate::wire::FrameReader::new(std::io::Cursor::new(body))
            .expect("the preamble reads");
        let mut rows_frames_seen = Vec::new();
        while let Some(frame) = reader.next_frame().expect("decodable") {
            if let dendro::replicate::Frame::Rows {
                seq,
                index_state,
                rows,
                ..
            } = frame
            {
                rows_frames_seen.push((seq, index_state, rows.len()));
            }
        }
        assert_eq!(
            rows_frames_seen.len(),
            4,
            "one rows frame per interval after the opening chunk: {rows_frames_seen:?}"
        );
        for (seq, index_state, rows) in &rows_frames_seen {
            assert_eq!(*rows, 0, "an interval with no reading sends no rows");
            assert_eq!(
                *index_state,
                dendro::replicate::NO_INDEX_STATE,
                "the agent keeps no identity index, so every frame names the no-index state \
                 (seq {seq})"
            );
        }
        for pair in rows_frames_seen.windows(2) {
            assert_eq!(
                pair[1].0,
                pair[0].0 + 1,
                "consecutive intervals, no gap: {rows_frames_seen:?}"
            );
        }
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
        let registration = Subscribers::new().register(Duration::from_secs(1));

        // A wall clock this test drives.
        let now = Arc::new(AtomicU64::new(1_700_000_000_000_000_000));

        let stream = subscription_body(
            subscription(Layout::Wide),
            builder,
            registration,
            wall_clock(&now),
        );
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

    /// GO check 2 of metriken's `docs/journal/2026-10-11-stream-route.md`:
    /// three subscriptions at one interval share one sampling pass per
    /// interval.
    ///
    /// Real clock, since the claim is about timers firing on the same
    /// boundaries and meeting the TTL cache, which a paused clock would
    /// decide by construction. Every frame for one interval (`seq`), across
    /// the three subscriptions, carries rows from one pass: one row
    /// timestamp per `seq`. A subscriber that reads the instant after
    /// waiting for the lock behind a slow pass finds that pass stale and
    /// samples again, which gives one `seq` several timestamps. Each
    /// subscription is sent `TICKS` frames, and the sampling passes are also
    /// at most the distinct intervals those frames name. A TTL below the
    /// interval (`1ms`) fails both.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_subscriptions_at_one_interval_share_one_pass_per_interval() {
        const TICKS: usize = 20;
        let interval = Duration::from_millis(50);
        let config: Config =
            toml::from_str("[general]\nttl = \"50ms\"\nsnapshot_format = \"v3\"\n")
                .expect("valid config");
        let builder = Arc::new(Mutex::new(SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        )));
        // The first pass builds every group's schema from the global
        // registry, which can take longer than an interval; done before
        // counting, as the HTTP tests above do.
        builder.lock().await.rows_at(Instant::now()).await;
        let before = builder.lock().await.samples();

        let subscribers = Subscribers::new();
        let streams: Vec<_> = (0..3)
            .map(|_| {
                let subscription = StreamSubscription::new(
                    StreamRequest {
                        interval,
                        layout: Layout::Wide,
                    },
                    interval,
                    Default::default(),
                    Default::default(),
                );
                subscription_body(
                    subscription,
                    Arc::clone(&builder),
                    subscribers.register(interval),
                    std::time::SystemTime::now,
                )
            })
            .collect();
        let tasks: Vec<_> = streams
            .into_iter()
            .map(|mut stream| {
                tokio::spawn(async move {
                    let mut body = Vec::new();
                    // The opening, then one chunk per interval.
                    for _ in 0..=TICKS {
                        let chunk = tokio::time::timeout(Duration::from_secs(10), stream.next())
                            .await
                            .expect("a frame inside the timeout")
                            .expect("the stream yields")
                            .expect("a chunk");
                        body.extend_from_slice(&chunk);
                    }
                    body
                })
            })
            .collect();

        let mut stamps: std::collections::BTreeMap<u64, std::collections::BTreeSet<i64>> =
            Default::default();
        for task in tasks {
            let body = task.await.unwrap();
            let mut reader = dendro::replicate::wire::FrameReader::new(std::io::Cursor::new(body))
                .expect("the preamble reads");
            let mut frames = 0;
            while let Some(frame) = reader.next_frame().expect("decodable") {
                if let dendro::replicate::Frame::Rows { seq, rows, .. } = frame {
                    let at = stamps.entry(seq).or_default();
                    at.extend(rows.iter().map(|row| row.ts));
                    frames += 1;
                }
            }
            assert_eq!(frames, TICKS, "one rows frame per interval");
        }
        for (seq, at) in &stamps {
            assert!(
                at.len() <= 1,
                "seq {seq} carries rows from {} passes: {at:?}",
                at.len()
            );
        }
        let passes = builder.lock().await.samples() - before;
        let intervals = stamps.len() as u64;
        assert!(
            passes <= intervals,
            "{passes} sampling passes for {intervals} distinct intervals of three subscriptions"
        );
        assert!(passes > 0, "the subscriptions sampled");
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

    /// A `.dendro` recording of an agent that cannot serve the stream fails
    /// loudly, and the two ways an agent can fail to serve it both classify
    /// as `Unsupported`: retrying will not change either answer, so the
    /// recorder must not sit in its retry loop on them.
    #[tokio::test]
    async fn an_agent_that_cannot_serve_the_stream_is_refused_as_unsupported() {
        use crate::recorder::stream::{ConnectError, Subscription};
        let client = test_client();

        // An agent from before the route existed: 404.
        let old = Router::new().route("/", get(root));
        let base = serve(old).await;
        match Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
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
        match Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
            Err(ConnectError::Unsupported(e)) => {
                assert!(e.contains("V2"), "says what kind of agent this is: {e}");
            }
            Err(ConnectError::Unreachable(e)) => panic!("a 409 is an answer, not an outage: {e}"),
            Ok(_) => panic!("a V2 agent has nothing to stream"),
        }
    }

    /// A 5xx is the path between here and the agent answering, not the
    /// agent: a proxy says 502 while the agent behind it restarts, which is
    /// exactly when a stream has just dropped and the pump is reconnecting.
    /// Classifying it as `Unsupported` made that restart end the recording.
    #[tokio::test]
    async fn a_server_error_is_an_outage_not_a_refusal() {
        use crate::recorder::stream::{ConnectError, Subscription};
        let proxy_down = Router::new().route(
            "/metrics/stream",
            get(|| async { (axum::http::StatusCode::SERVICE_UNAVAILABLE, "upstream down") }),
        );
        let base = serve(proxy_down).await;
        match Subscription::connect(
            &test_client(),
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
            Err(ConnectError::Unreachable(e)) => assert!(e.contains("503"), "{e}"),
            Err(ConnectError::Unsupported(e)) => {
                panic!("a 503 will change on retry and must not end the run: {e}")
            }
            Ok(_) => panic!("there is no stream behind a 503"),
        }
    }

    /// Serve `body` as a replication stream's bytes on `/metrics/stream`.
    async fn serve_stream_bytes(body: Vec<u8>) -> reqwest::Url {
        use axum::response::IntoResponse;
        let router = Router::new().route(
            "/metrics/stream",
            get(move || {
                let body = body.clone();
                async move {
                    (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            axum::http::HeaderValue::from_static(
                                metriken_exposition::stream::WIRE_NAMES.content_type,
                            ),
                        )],
                        body,
                    )
                        .into_response()
                }
            }),
        );
        serve(router).await
    }

    /// Serve a stream's opening (preamble and handshake) on
    /// `/metrics/stream` with `headers`, `Content-Type` included.
    async fn serve_opening_with(headers: Vec<(&'static str, &'static str)>) -> reqwest::Url {
        use axum::response::IntoResponse;
        let mut opening = Vec::new();
        dendro::replicate::wire::write_preamble(&mut opening).unwrap();
        let mut producer = metriken_exposition::stream::FrameProducer::for_source(
            "epoch-names".to_string(),
            crate::agent::epoch::clock_anchor_wall_ns(),
            Default::default(),
            Default::default(),
        );
        dendro::replicate::wire::encode_frame(&producer.handshake(), &mut opening).unwrap();
        let router = Router::new().route(
            "/metrics/stream",
            get(move || {
                let (headers, body) = (headers.clone(), opening.clone());
                async move {
                    let mut map = axum::http::HeaderMap::new();
                    for (name, value) in headers {
                        map.insert(
                            axum::http::HeaderName::from_static(name),
                            axum::http::HeaderValue::from_static(value),
                        );
                    }
                    (map, body).into_response()
                }
            }),
        );
        serve(router).await
    }

    /// The agent serves metriken's wire names and none of the legacy ones.
    #[tokio::test]
    async fn the_agent_serves_the_metriken_wire_names() {
        use metriken_exposition::stream::WIRE_NAMES;
        let state = test_state("[general]\nttl = \"1s\"\nsnapshot_format = \"v3\"\n");
        let base = serve(app(state)).await;
        let response = test_client()
            .get(
                base.join("/metrics/stream?interval=500ms&layout=long")
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        assert_eq!(
            header("content-type").as_deref(),
            Some(WIRE_NAMES.content_type)
        );
        assert_eq!(header(WIRE_NAMES.frame_interval).as_deref(), Some("500ms"));
        assert_eq!(header(WIRE_NAMES.update_floor).as_deref(), Some("1s"));
        assert_eq!(header(WIRE_NAMES.layout).as_deref(), Some("long"));
        let legacy: Vec<_> = response
            .headers()
            .keys()
            .filter(|k| k.as_str().starts_with("x-rezolus-"))
            .collect();
        assert!(legacy.is_empty(), "no legacy headers: {legacy:?}");
    }

    /// The recorder matches the `Content-Type` against each accepted set of
    /// names and reads that set's headers, and only that set's: a stream
    /// under metriken's content type with legacy headers has no floor and is
    /// read as wide. A content type no set has is refused, naming the ones
    /// accepted.
    #[tokio::test]
    async fn connect_reads_the_headers_of_the_names_its_content_type_matched() {
        use crate::recorder::stream::{
            ConnectError, Subscription, ACCEPTED_WIRE_NAMES, REZOLUS_LEGACY_WIRE_NAMES,
        };
        use metriken_exposition::stream::WIRE_NAMES;
        let client = test_client();
        let connect = |base: reqwest::Url| {
            let client = client.clone();
            async move {
                Subscription::connect(
                    &client,
                    &base,
                    Duration::from_secs(1),
                    Layout::Long,
                    ACCEPTED_WIRE_NAMES,
                )
                .await
            }
        };

        for names in [WIRE_NAMES, REZOLUS_LEGACY_WIRE_NAMES] {
            let base = serve_opening_with(vec![
                ("content-type", names.content_type),
                (names.update_floor, "5s"),
                (names.layout, "long"),
            ])
            .await;
            let sub = connect(base).await.expect("subscribes");
            assert_eq!(
                sub.update_floor(),
                Some(Duration::from_secs(5)),
                "{}",
                names.content_type
            );
            assert_eq!(sub.layout(), Layout::Long, "{}", names.content_type);
        }

        let crossed = serve_opening_with(vec![
            ("content-type", WIRE_NAMES.content_type),
            (REZOLUS_LEGACY_WIRE_NAMES.update_floor, "5s"),
            (REZOLUS_LEGACY_WIRE_NAMES.layout, "long"),
        ])
        .await;
        let sub = connect(crossed).await.expect("subscribes");
        assert_eq!(sub.update_floor(), None);
        assert_eq!(sub.layout(), Layout::Wide);

        let other = serve_opening_with(vec![("content-type", "application/octet-stream")]).await;
        match connect(other).await {
            Err(ConnectError::Unsupported(e)) => {
                assert!(e.contains(WIRE_NAMES.content_type), "{e}");
                assert!(e.contains(REZOLUS_LEGACY_WIRE_NAMES.content_type), "{e}");
            }
            Err(ConnectError::Unreachable(e)) => panic!("retrying cannot fix this: {e}"),
            Ok(_) => panic!("not a replication stream"),
        }
    }

    /// Bytes that do not decode as a handshake are refused, not retried: the
    /// agent will send the same bytes next time. A protocol-version mismatch
    /// names both versions, so a version skew between agent and recorder
    /// builds can be read off the message.
    #[tokio::test]
    async fn a_malformed_handshake_is_refused_by_what_was_wrong() {
        use crate::recorder::stream::{ConnectError, Subscription};
        use dendro::replicate::wire::{MAGIC, PROTOCOL_VERSION};
        let client = test_client();

        let mut other_version = MAGIC.to_vec();
        let theirs = PROTOCOL_VERSION.wrapping_add(1);
        other_version.extend_from_slice(&theirs.to_le_bytes());
        let base = serve_stream_bytes(other_version).await;
        match Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
            Err(ConnectError::Unsupported(e)) => {
                assert!(e.contains(&format!("version {theirs}")), "{e}");
                assert!(e.contains(&format!("version {PROTOCOL_VERSION}")), "{e}");
            }
            Err(ConnectError::Unreachable(e)) => panic!("retrying cannot fix this: {e}"),
            Ok(_) => panic!("not this protocol"),
        }

        let base = serve_stream_bytes(b"not a replication stream".to_vec()).await;
        match Subscription::connect(
            &client,
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
            Err(ConnectError::Unsupported(e)) => assert!(e.contains("magic"), "{e}"),
            Err(ConnectError::Unreachable(e)) => panic!("retrying cannot fix this: {e}"),
            Ok(_) => panic!("not this protocol"),
        }
    }

    /// A connection that ends during the handshake is an outage: the next
    /// attempt may complete it.
    #[tokio::test]
    async fn a_handshake_cut_short_is_an_outage_not_a_refusal() {
        use crate::recorder::stream::{ConnectError, Subscription};
        use dendro::replicate::wire::{MAGIC, PROTOCOL_VERSION};
        let mut preamble_only = MAGIC.to_vec();
        preamble_only.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        let base = serve_stream_bytes(preamble_only).await;
        match Subscription::connect(
            &test_client(),
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
            Err(ConnectError::Unreachable(e)) => assert!(e.contains("handshake"), "{e}"),
            Err(ConnectError::Unsupported(e)) => panic!("a retry may complete it: {e}"),
            Ok(_) => panic!("no handshake arrived"),
        }
    }

    /// A connection that stays open and says nothing is dead, whatever the
    /// kernel thinks: a peer that vanished without a RST never closes it. The
    /// agent sends a frame every interval, empty when nothing is new, so a
    /// silence longer than the timeout is the signal. Without the bound, the
    /// pump sat in the read for the rest of the run and the recording ended
    /// early with no warning.
    #[tokio::test]
    async fn a_stream_that_goes_silent_after_its_handshake_is_reported_dropped() {
        use crate::recorder::stream::{pump, StreamEvent, Subscription};

        // The opening bytes of a real stream, then nothing, forever.
        let silent = Router::new().route(
            "/metrics/stream",
            get(|| async {
                let mut opening = Vec::new();
                dendro::replicate::wire::write_preamble(&mut opening).unwrap();
                let mut producer = metriken_exposition::stream::FrameProducer::for_source(
                    "epoch-silent".to_string(),
                    crate::agent::epoch::clock_anchor_wall_ns(),
                    Default::default(),
                    Default::default(),
                );
                dendro::replicate::wire::encode_frame(&producer.handshake(), &mut opening).unwrap();
                let body = axum::body::Body::from_stream(async_stream::stream! {
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(opening));
                    std::future::pending::<()>().await;
                });
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static(
                            metriken_exposition::stream::WIRE_NAMES.content_type,
                        ),
                    )],
                    body,
                )
            }),
        );
        let base = serve(silent).await;
        let client = test_client();
        let sub = Subscription::connect(
            &client,
            &base,
            Duration::from_millis(100),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        .expect("the handshake arrives, so the connect succeeds");

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(pump(
            0,
            sub,
            client,
            base,
            Duration::from_millis(100),
            crate::recorder::stream::Layout::Wide,
            Duration::from_millis(500),
            tx,
        ));
        let (_, event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the silence is noticed inside the timeout")
            .expect("the pump is alive");
        match event {
            StreamEvent::Dropped(reason) => {
                assert!(reason.contains("no frame"), "{reason}");
            }
            other => panic!("a silent stream must be reported dropped, got {other:?}"),
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
        match Subscription::connect(
            &test_client(),
            &base,
            Duration::from_secs(1),
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        {
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

        let sub = Subscription::connect(
            &client,
            &base,
            interval,
            crate::recorder::stream::Layout::Wide,
            crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        )
        .await
        .expect("subscribes");
        let first = sub.source().cloned().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(pump(
            3,
            sub,
            client.clone(),
            base.clone(),
            interval,
            crate::recorder::stream::Layout::Wide,
            Duration::from_secs(10),
            tx,
        ));

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

        next_matching(&mut rx, |e| match e {
            StreamEvent::Interval(a) => Some(a),
            _ => None,
        })
        .await;

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
        next_matching(&mut rx, |e| match e {
            StreamEvent::Interval(a) => Some(a),
            _ => None,
        })
        .await;
    }
}

/// GO check 1 of metriken's `docs/journal/2026-10-11-stream-route.md`.
///
/// metriken-exposition's [`StreamSubscription`] is driven through one fixed
/// sequence of synthetic passes, in each layout, with the agent's own pass
/// types (`AgentRows` wide, `StreamRows` long), and its output is compared
/// byte for byte with a committed fixture under
/// `tests/fixtures/stream-frames/`. The fixtures were recorded from the
/// agent's own frame code before the route moved to metriken-exposition
/// (#1434), so a match says the move changed no frame.
///
/// File format: one record per chunk the stream yields, the opening chunk
/// (preamble and handshake) first, then one per interval. Each record is a
/// little-endian `u32` byte length followed by the chunk's bytes.
///
/// Regenerate with `REZOLUS_BLESS=1 cargo test stream_frame_fixture`, and only
/// when the frames are meant to change.
#[cfg(test)]
mod stream_frame_fixture {
    use super::*;
    use crate::recorder::schema::{GroupSchema, MetricDesc};
    use crate::recorder::wire::{AgentRow, AgentRows};
    use metriken_exposition::stream::{
        EncodedGroup, EncodedLongGroup, EncodedStreamGroup, Pass, StreamRequest,
    };
    use metriken_storage::wal::{encode_wal_long_row, LongOccupant, WalLongRow};
    use std::collections::BTreeMap;

    const UUID: &str = "0f1e2d3c-4b5a-4968-8776-655443322110";
    const ANCHOR: i64 = 1_700_000_000_000_000_000;
    const SECOND: u64 = 1_000_000_000;
    /// Subscription wake times are offsets from this, half an interval past
    /// a boundary so `seq` does not sit on an edge.
    const BASE: u64 = 1_700_000_000_000_000_000 + SECOND / 2;

    fn subscription(layout: Layout) -> StreamSubscription {
        StreamSubscription::for_source(
            StreamRequest {
                interval: Duration::from_secs(1),
                layout,
            },
            Duration::from_secs(1),
            UUID.to_string(),
            ANCHOR,
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [(dendro::keys::PRODUCER_EPOCH.to_string(), UUID.to_string())]
                .into_iter()
                .collect(),
        )
    }

    fn schema(n: usize) -> GroupSchema {
        GroupSchema {
            counters: (0..n)
                .map(|i| MetricDesc {
                    name: format!("0x{i}"),
                    metadata: [("metric".to_string(), format!("counter_{i}"))]
                        .into_iter()
                        .collect(),
                })
                .collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        }
    }

    /// One group of a pass: its stream, window, counter count, and the value
    /// its counters start from.
    struct G {
        stream: &'static str,
        window: Option<(u64, u64)>,
        n: usize,
        base: u64,
    }

    /// A pass's stamp: `wall_ns`, `ts`, `wall_offset`.
    type Stamp = (u64, i64, i64);

    fn wide_row(g: &G) -> AgentRow {
        let schema = schema(g.n);
        let row = crate::recorder::wal::encode_wal_group_row(&crate::recorder::wal::WalGroupRow {
            schema_hash: schema.hash(),
            schema: None,
            window: g.window,
            counters: (0..g.n).map(|i| Some(g.base + i as u64)).collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
        .unwrap();
        AgentRow {
            stream: g.stream.to_string(),
            window: g.window,
            schema_hash: schema.hash(),
            schema: Some(Arc::new(schema)),
            arity: (g.n as u32, 0, 0),
            approx_bytes: row.len() as u32,
            row,
        }
    }

    fn wide_pass(stamp: Stamp, groups: &[G]) -> Arc<AgentRows> {
        Arc::new(AgentRows {
            wall_ns: stamp.0,
            ts: stamp.1,
            wall_offset: stamp.2,
            duration_ns: 1_000,
            rows: groups.iter().map(wide_row).collect(),
        })
    }

    /// A windowed group in the long form, with `g.n` occupants of one counter
    /// each. A windowless group is built wide, so the long sequence mixes
    /// both forms.
    fn long_group(g: &G) -> EncodedStreamGroup {
        if g.window.is_none() {
            let row = wide_row(g);
            return EncodedStreamGroup::Wide(EncodedGroup {
                stream: row.stream,
                window: row.window,
                schema_hash: row.schema_hash,
                schema: row.schema,
                row: row.row,
            });
        }
        let columns = schema(1);
        let keys: Vec<u64> = (0..g.n as u64).map(|i| 0x100 + i).collect();
        let row = encode_wal_long_row(&WalLongRow {
            schema_hash: columns.hash(),
            schema: None,
            window: g.window,
            occupants: keys
                .iter()
                .map(|&key| LongOccupant {
                    occupant: key,
                    counters: vec![Some(g.base + key)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                })
                .collect(),
        })
        .unwrap();
        EncodedStreamGroup::Long(EncodedLongGroup {
            stream: g.stream.to_string(),
            window: g.window,
            columns_hash: columns.hash(),
            columns: Arc::new(columns),
            occupants: keys
                .iter()
                .map(|&key| {
                    let labels: BTreeMap<String, String> =
                        [("id".to_string(), key.to_string())].into_iter().collect();
                    (key, Arc::new(labels))
                })
                .collect(),
            row,
        })
    }

    fn long_pass(stamp: Stamp, groups: &[G]) -> Arc<snapshot::StreamRows> {
        Arc::new(snapshot::StreamRows {
            wall_ns: stamp.0,
            ts: stamp.1,
            wall_offset: stamp.2,
            rows: groups.iter().map(long_group).collect(),
        })
    }

    fn g(stream: &'static str, window: Option<(u64, u64)>, n: usize, base: u64) -> G {
        G {
            stream,
            window,
            n,
            base,
        }
    }

    /// The sequence, as `(wake time, pass)`. `pass` builds the layout's
    /// pass from a stamp and groups.
    ///
    /// 1. First pass: `cpu/a` and `cpu/b` windowed, `external/main`
    ///    windowless. All sent.
    /// 2. The same pass again (same stamp): an empty frame.
    /// 3. A new pass where `cpu/a`'s window did not advance and `cpu/b`'s did:
    ///    `cpu/b` and `external/main` sent.
    /// 4. No pass: an empty frame.
    /// 5. Two seconds later, so `seq` skips one: both windows advanced, and
    ///    `cpu/b` has one more counter (wide: a new schema; long: a new
    ///    occupant).
    /// 6. The wall clock steps back half a second, same pass: an empty frame
    ///    whose `seq` is held at the previous one plus one.
    fn sequence<P: Pass>(pass: fn(Stamp, &[G]) -> P) -> Vec<(u64, Option<P>)> {
        let s1: Stamp = (BASE + SECOND - 10_000, ANCHOR + 1_000_000, 7);
        let s2: Stamp = (BASE + 3 * SECOND - 10_000, ANCHOR + 2_000_000, 7);
        let s3: Stamp = (BASE + 6 * SECOND - 10_000, ANCHOR + 3_000_000, -3);
        let p1 = || {
            pass(
                s1,
                &[
                    g("cpu/a", Some((100, 200)), 2, 10),
                    g("cpu/b", Some((100, 210)), 2, 20),
                    g("external/main", None, 1, 30),
                ],
            )
        };
        let p2 = pass(
            s2,
            &[
                g("cpu/a", Some((100, 200)), 2, 10),
                g("cpu/b", Some((210, 320)), 2, 21),
                g("external/main", None, 1, 31),
            ],
        );
        let p3 = || {
            pass(
                s3,
                &[
                    g("cpu/a", Some((200, 400)), 2, 12),
                    g("cpu/b", Some((320, 430)), 3, 22),
                    g("external/main", None, 1, 32),
                ],
            )
        };
        vec![
            (BASE + SECOND, Some(p1())),
            (BASE + 2 * SECOND, Some(p1())),
            (BASE + 3 * SECOND, Some(p2)),
            (BASE + 4 * SECOND, None),
            (BASE + 6 * SECOND, Some(p3())),
            (BASE + 6 * SECOND - SECOND / 2 - 1, Some(p3())),
        ]
    }

    /// Every chunk the subscription yields for `sequence`, opening first.
    fn chunks<P: Pass>(layout: Layout, pass: fn(Stamp, &[G]) -> P) -> Vec<bytes::Bytes> {
        let mut subscription = subscription(layout);
        let mut out = vec![subscription.opening().unwrap()];
        for (wall_ns, pass) in sequence(pass) {
            let woke = std::time::UNIX_EPOCH + Duration::from_nanos(wall_ns);
            out.push(subscription.frame_at(pass.as_ref(), woke).unwrap());
        }
        out
    }

    fn serialize(chunks: &[bytes::Bytes]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out
    }

    /// `(seq, streams)` of each rows frame, decoded from the concatenated
    /// chunks with dendro's reader. Checked as well as the bytes, so a fixture
    /// blessed from a broken rule does not pass on the strength of matching
    /// itself.
    fn decoded(chunks: &[bytes::Bytes]) -> Vec<(u64, Vec<String>)> {
        let body: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        let mut reader = dendro::replicate::wire::FrameReader::new(std::io::Cursor::new(body))
            .expect("the preamble reads");
        let mut out = Vec::new();
        while let Some(frame) = reader.next_frame().expect("decodable") {
            if let dendro::replicate::Frame::Rows { seq, rows, .. } = frame {
                out.push((seq, rows.into_iter().map(|r| r.stream).collect()));
            }
        }
        out
    }

    fn check<P: Pass>(
        name: &str,
        layout: Layout,
        pass: fn(Stamp, &[G]) -> P,
        expected: &[(u64, &[&str])],
    ) {
        let chunks = chunks(layout, pass);

        // `seq` is the index of the one-second interval woken in.
        let first = (BASE + SECOND) / SECOND;
        let expected: Vec<(u64, Vec<String>)> = expected
            .iter()
            .map(|(seq, streams)| (first + seq, streams.iter().map(|s| s.to_string()).collect()))
            .collect();
        assert_eq!(
            decoded(&chunks),
            expected,
            "{name}: frames by seq and stream"
        );

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stream-frames")
            .join(format!("{name}.bin"));
        let bytes = serialize(&chunks);
        if std::env::var("REZOLUS_BLESS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            return;
        }
        let fixture = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{}: {e}; regenerate with REZOLUS_BLESS=1", path.display()));
        assert!(
            fixture == bytes,
            "{name}: frames differ from {} ({} bytes produced, {} in the fixture)",
            path.display(),
            bytes.len(),
            fixture.len()
        );
    }

    #[test]
    fn wide_frames_match_the_fixture() {
        check(
            "wide",
            Layout::Wide,
            wide_pass,
            &[
                (0, &["cpu/a", "cpu/b", "external/main"]),
                (1, &[]),
                (2, &["cpu/b", "external/main"]),
                (3, &[]),
                (5, &["cpu/a", "cpu/b", "external/main"]),
                (6, &[]),
            ],
        );
    }

    #[test]
    fn long_frames_match_the_fixture() {
        // A long group's new occupants go out first, on `<group>/occupants`.
        check(
            "long",
            Layout::Long,
            long_pass,
            &[
                (
                    0,
                    &[
                        "cpu/a/occupants",
                        "cpu/a",
                        "cpu/b/occupants",
                        "cpu/b",
                        "external/main",
                    ],
                ),
                (1, &[]),
                (2, &["cpu/b", "external/main"]),
                (3, &[]),
                (5, &["cpu/a", "cpu/b/occupants", "cpu/b", "external/main"]),
                (6, &[]),
            ],
        );
    }
}
