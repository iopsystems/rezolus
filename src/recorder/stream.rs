//! Consuming an agent's replication stream.
//!
//! The agent produces `Frame::{Handshake, Rows}` over HTTP
//! (`/metrics/stream`); this turns them back into the passes a `.dendro`
//! recording ingests.
//!
//! # Why not dendro's `Subscriber`
//!
//! dendro ships one. It owns a dendro `Writer`, while `record -o out.dendro` writes
//! through metriken-archive's `ArchiveWriter`, which maps the producer's
//! occupant keys to its own occupant numbers (`SourceRecorder::stage_streamed`).
//! So the frames are read and checked here; the callers decode their rows
//! with `StreamDecoder`, and metriken-archive writes the archive.
//!
//! # Layout
//!
//! A subscription asks for a [`Layout`]. In the long layout a group of slots
//! arrives as values keyed by occupant, with each occupant's labels sent
//! once on `<group>/occupants`; in the wide layout every group's schema
//! lists every member. `StreamDecoder` reads either, so an agent that serves
//! only the wide layout is still recorded.
//!
//! # No identity index
//!
//! Identity travels in the rows, `__uid__` included. A 6.0 agent sends no
//! `Frame::Index` and stamps every rows frame with dendro's `NO_INDEX_STATE`.
//! An older agent still sends index frames and names its own index state;
//! both are ignored, since the same labels arrive in the rows.

use dendro::archive::WalRow;
use dendro::replicate::Frame;
use std::time::Duration;

/// What one interval's frames amounted to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The interval index the producer stamped. Not a frame count: a gap says
    /// an interval the subscriber asked for produced no reading.
    pub seq: u64,
    /// The interval's rows, ready for the writer.
    pub rows: Vec<WalRow>,
    /// Whether `seq` jumped, meaning intervals produced no frame at all.
    pub gap: bool,
}

/// How a stream carries groups whose metrics are all counter or gauge
/// groups: as a `WalGroupRow` whose schema lists every member (wide), or as
/// a `WalLongRow` keyed by occupant with each occupant's labels sent on
/// `<group>/occupants` (long). See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Wide,
    Long,
}

impl Layout {
    /// The value of the `layout` query parameter and `x-rezolus-layout`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Layout::Wide => "wide",
            Layout::Long => "long",
        }
    }
}

/// One pass off the stream: the producer's stamp and its rows, in the order
/// they arrived.
#[derive(Debug)]
pub(crate) struct Pass {
    pub ts: i64,
    pub wall_offset: i64,
    pub rows: Vec<WalRow>,
}

impl Applied {
    /// Turn the rows into passes, one [`Pass`] per distinct stamp.
    ///
    /// For a well-formed interval that is exactly one: the producer stamps a
    /// whole pass once. Grouping rather than assuming keeps a relay that
    /// batches several passes into one frame from landing them all on the
    /// first pass's stamp. By `ts` alone, not `(ts, wall_offset)`: two passes
    /// at one `ts` would write one row's worth of key however their wall
    /// offsets differ. The first row's `wall_offset` stands for the pass.
    ///
    /// A negative stamp is refused for the reason `snapshot_producer_stamp`
    /// refuses one: the archive's `ts` is unsigned, and a producer that sent
    /// one is not one to guess for.
    pub(crate) fn for_writer(self) -> Result<Vec<Pass>, String> {
        let mut by_stamp: std::collections::BTreeMap<i64, (i64, Vec<WalRow>)> =
            std::collections::BTreeMap::new();
        for row in self.rows {
            if row.ts < 0 {
                return Err(format!(
                    "stream {} stamped a row at {} ns, before the epoch",
                    row.stream, row.ts
                ));
            }
            by_stamp
                .entry(row.ts)
                .or_insert_with(|| (row.wall_offset, Vec::new()))
                .1
                .push(row);
        }
        Ok(by_stamp
            .into_iter()
            .map(|(ts, (wall_offset, rows))| Pass {
                ts,
                wall_offset,
                rows,
            })
            .collect())
    }
}

/// The source a stream is carrying, from its handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub uuid: Option<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub metadata: std::collections::BTreeMap<String, String>,
    /// The producer's anchor. Row timestamps are on this timeline, so a
    /// recording that discarded it could not place its own rows in wall time.
    pub clock_anchor_wall_ns: i64,
}

/// Accumulates one connection's frames.
#[derive(Debug, Default)]
pub(crate) struct StreamSubscriber {
    source: Option<Source>,
    last_seq: Option<u64>,
}

impl StreamSubscriber {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The source this stream is carrying, once its handshake has arrived.
    pub(crate) fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// Fold one interval's frames in.
    ///
    /// Takes the frames of a whole interval: everything up to and including
    /// its `Rows`.
    pub(crate) fn apply(&mut self, frames: Vec<Frame>) -> Result<Applied, String> {
        let mut out = Applied::default();

        for frame in frames {
            match frame {
                Frame::Handshake {
                    uuid,
                    labels,
                    metadata,
                    clock_anchor_wall_ns,
                    ..
                } => {
                    // A second handshake on one connection would mean the
                    // producer restarted underneath us, which is a new source
                    // and a new timeline rather than more of this one.
                    if let Some(existing) = &self.source {
                        if existing.uuid != uuid {
                            return Err(format!(
                                "the stream changed source mid-connection ({:?} -> {uuid:?}); \
                                 its rows are on a different timeline and cannot be appended \
                                 to this recording",
                                existing.uuid
                            ));
                        }
                    }
                    self.source = Some(Source {
                        uuid,
                        labels,
                        metadata,
                        clock_anchor_wall_ns,
                    });
                }

                // An agent before 6.0 sends an identity index. Its labels are
                // also in the rows' schemas, which is where a `.dendro` takes
                // identity from, so the frame is dropped.
                Frame::Index { .. } => {}

                // `index_state` is not checked: a 6.0 agent always names
                // `NO_INDEX_STATE`, and an older agent's state refers to an
                // index this subscriber does not keep.
                Frame::Rows { seq, rows, .. } => {
                    if let Some(last) = self.last_seq {
                        out.gap = seq > last.saturating_add(1);
                    }
                    self.last_seq = Some(seq);
                    out.seq = seq;
                    out.rows.extend(rows);
                }

                // Segments and clock offsets are an archive publisher's
                // frames; a live agent has no archive to send them from. A
                // stream that produced one is not this protocol, and guessing
                // at it would put rows in a recording that nothing accounted
                // for.
                other => {
                    return Err(format!(
                        "unexpected frame on an agent stream: {}",
                        frame_kind(&other)
                    ))
                }
            }
        }

        Ok(out)
    }
}

/// Names a frame for an error message.
///
/// It needs a catch-all arm: dendro 0.3.0 made `Frame` `#[non_exhaustive]`, so
/// a variant added to it no longer stops this compiling. Nothing is moved past
/// silently because of that. The subscriber refuses every frame it was not
/// written for, at runtime ("unexpected frame on an agent stream"), and
/// `FrameDecoder` decodes with `decode_payload`, which refuses a kind the
/// linked dendro does not know rather than skipping it as dendro's own
/// `FrameReader` does. A new kind is therefore still a decision someone has to
/// make on the recorder's side; it is found when a stream carries one rather
/// than when this is compiled.
fn frame_kind(frame: &Frame) -> &'static str {
    match frame {
        Frame::Handshake { .. } => "handshake",
        Frame::Index { .. } => "index",
        Frame::Rows { .. } => "rows",
        Frame::Segment { .. } => "segment",
        Frame::ClockOffset { .. } => "clock offset",
        _ => "kind this recorder was not written for",
    }
}

/// Frames out of a byte stream that arrives in pieces.
///
/// dendro's `FrameReader` wants a `Read`, and an HTTP response body is an
/// async stream of chunks that respects no frame boundary — one chunk may hold
/// three frames, or a third of one. This holds the remainder between chunks and
/// yields whole frames as they complete, using the same length prefix the
/// encoder writes.
///
/// It is the first consumer of `MAGIC`, `PROTOCOL_VERSION`,
/// `LENGTH_PREFIX_BYTES` and `MAX_FRAME_BYTES`, which exist for exactly this:
/// pairing a reader that owns its source with a caller that does not.
#[derive(Debug, Default)]
pub(crate) struct FrameDecoder {
    buf: Vec<u8>,
    preamble_read: bool,
}

impl FrameDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a chunk and take whatever frames it completed.
    ///
    /// An empty result is normal: a chunk that does not finish a frame
    /// produces nothing and is not an error.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>, String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();

        if !self.preamble_read {
            let want = dendro::replicate::wire::MAGIC.len() + 2;
            if self.buf.len() < want {
                return Ok(out);
            }
            let magic = &self.buf[..dendro::replicate::wire::MAGIC.len()];
            if magic != dendro::replicate::wire::MAGIC {
                // Named rather than shrugged at: the overwhelmingly likely
                // cause is an endpoint serving the older msgpack stream, or
                // something that is not this endpoint at all, and a decoder
                // that limped on would report malformed frames forever
                // instead of the one fact that explains them.
                return Err(
                    "the stream does not begin with dendro's replication magic; this is \
                     not a replication stream"
                        .to_string(),
                );
            }
            let version = u16::from_le_bytes([
                self.buf[dendro::replicate::wire::MAGIC.len()],
                self.buf[dendro::replicate::wire::MAGIC.len() + 1],
            ]);
            if version != dendro::replicate::wire::PROTOCOL_VERSION {
                return Err(format!(
                    "the agent sent replication protocol version {version}; this recorder \
                     implements version {}. The agent and recorder builds do not match",
                    dendro::replicate::wire::PROTOCOL_VERSION
                ));
            }
            self.buf.drain(..want);
            self.preamble_read = true;
        }

        loop {
            const PREFIX: usize = dendro::replicate::wire::LENGTH_PREFIX_BYTES;
            if self.buf.len() < PREFIX {
                break;
            }
            let len =
                u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            if len > dendro::replicate::wire::MAX_FRAME_BYTES {
                // A corrupt or hostile prefix is otherwise an allocation of up
                // to 4 GiB, which is an out-of-memory rather than an error.
                return Err(format!(
                    "a frame declares {len} bytes, past the {} byte limit",
                    dendro::replicate::wire::MAX_FRAME_BYTES
                ));
            }
            if self.buf.len() < PREFIX + len {
                break;
            }
            let frame = dendro::replicate::wire::decode_payload(&self.buf[PREFIX..PREFIX + len])
                .map_err(|e| format!("undecodable replication frame: {e}"))?;
            self.buf.drain(..PREFIX + len);
            out.push(frame);
        }

        Ok(out)
    }

    /// Bytes held back because they do not yet complete a frame. A stream that
    /// ended with some is a truncated stream, which a caller may want to say.
    pub(crate) fn pending(&self) -> usize {
        self.buf.len()
    }
}

/// Why a subscription could not be opened, split by whether trying again
/// could change the answer.
///
/// The recorder treats the two differently on purpose. An agent that is not
/// there yet is the ordinary "will retry each tick" case scraping already has.
/// An agent that answered and cannot serve the stream — no route (404), no
/// acquisition groups to stream (the 409 a V2 agent gives), a body of the
/// wrong type — is a configuration the run was not written for, and it fails
/// loudly rather than falling back to scraping: a `.dendro` records agents by
/// stream only, and a run that silently scraped one would put two endpoints of
/// one A/B on different transports.
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// Nothing answered, or the connection dropped before the handshake.
    Unreachable(String),
    /// Something answered, and it cannot serve a replication stream.
    Unsupported(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unreachable(e) | ConnectError::Unsupported(e) => f.write_str(e),
        }
    }
}

/// Why reading the connection failed: the transport, which retrying can
/// change, or bytes that do not decode, which it cannot.
#[derive(Debug)]
enum FillError {
    Transport(String),
    Decode(String),
}

impl FillError {
    fn into_message(self) -> String {
        match self {
            FillError::Transport(e) | FillError::Decode(e) => e,
        }
    }
}

/// One live subscription to an agent.
///
/// Owns the connection, the decoder and the subscriber, and hands back one
/// interval at a time.
pub(crate) struct Subscription {
    response: reqwest::Response,
    decoder: FrameDecoder,
    subscriber: StreamSubscriber,
    /// Frames decoded but not yet part of a complete interval.
    pending: Vec<Frame>,
    /// The agent's `x-rezolus-update-floor`: how often a frame can carry
    /// anything new, which is its snapshot TTL. `None` when the agent did
    /// not say.
    update_floor: Option<Duration>,
    /// The layout the agent serves, from `x-rezolus-layout`; wide when it
    /// does not say, as an agent before the long layout does not.
    layout: Layout,
}

impl Subscription {
    /// Open a subscription asking for `interval`, and wait for its handshake.
    ///
    /// The interval is a request, not a guarantee: the agent's TTL is the
    /// floor on how often anything can be new, and it reports that floor in
    /// `x-rezolus-update-floor`. Asking for less is legal and gets the frames
    /// asked for, most of them empty.
    ///
    /// Returns only once the handshake has been applied, so
    /// [`source`](Self::source) is `Some` on every open subscription. The
    /// recorder needs the handshake's anchor to open the recording the rows
    /// will land in, and a first frame that is not a handshake is not this
    /// protocol — both are better learned here than one interval later.
    pub(crate) async fn connect(
        client: &reqwest::Client,
        base: &reqwest::Url,
        interval: Duration,
        layout: Layout,
    ) -> Result<Self, ConnectError> {
        let mut url = base.clone();
        url.set_path("/metrics/stream");
        let interval = humantime::format_duration(interval);
        url.set_query(Some(&match layout {
            Layout::Wide => format!("interval={interval}"),
            Layout::Long => format!("interval={interval}&layout={}", layout.as_str()),
        }));

        let response =
            client.get(url.clone()).send().await.map_err(|e| {
                ConnectError::Unreachable(format!("failed to subscribe to {url}: {e}"))
            })?;

        if !response.status().is_success() {
            // Which class a status falls in is what the recorder acts on: an
            // answer that will not change with retrying ends the run, one
            // that might is retried. 404 and 409 are the agent saying what it
            // is. A 5xx, 408 or 429 is the path between here and the agent —
            // a proxy answering while the agent behind it restarts is the
            // common case, and it is exactly the moment a stream has just
            // dropped and the pump is reconnecting.
            let code = response.status().as_u16();
            return Err(match code {
                409 => ConnectError::Unsupported(format!(
                    "{url} cannot serve a replication stream: the agent reports no \
                     acquisition groups, which a V2 agent never has"
                )),
                // Not "predates the stream": a current agent behind a proxy
                // that does not route this path answers the same way. The
                // caller knows the agent's version and says which it is.
                404 => ConnectError::Unsupported(format!("{url} returned HTTP 404")),
                408 | 429 | 500..=599 => {
                    ConnectError::Unreachable(format!("{url} returned HTTP {code}"))
                }
                _ => ConnectError::Unsupported(format!("{url} returned HTTP {code}")),
            });
        }

        // Checked before any bytes, so an agent serving the older msgpack
        // stream is named as that rather than as a stream of malformed frames.
        // The decoder's magic check would catch it too; this says it sooner
        // and more precisely.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type != crate::agent::REPLICATION_CONTENT_TYPE {
            return Err(ConnectError::Unsupported(format!(
                "{url} serves `{content_type}`, not `{}`; this recorder reads only `{}`",
                crate::agent::REPLICATION_CONTENT_TYPE,
                crate::agent::REPLICATION_CONTENT_TYPE
            )));
        }

        let update_floor = response
            .headers()
            .get("x-rezolus-update-floor")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| humantime::parse_duration(v).ok());

        let served = match response
            .headers()
            .get("x-rezolus-layout")
            .and_then(|v| v.to_str().ok())
        {
            Some("long") => Layout::Long,
            _ => Layout::Wide,
        };

        let mut sub = Self {
            response,
            decoder: FrameDecoder::new(),
            subscriber: StreamSubscriber::new(),
            pending: Vec::new(),
            update_floor,
            layout: served,
        };

        // The handshake is the first frame by protocol. Anything else first
        // is a producer this subscriber was not written for, and so are bytes
        // that do not decode (wrong magic, another protocol version, a frame
        // past the size limit): retrying gets the same bytes, so those refuse.
        // A connection that ends before or during the handshake is a
        // transport failure like any other.
        while sub.pending.is_empty() {
            let filled = sub.fill().await.map_err(|e| match e {
                FillError::Transport(e) => ConnectError::Unreachable(e),
                FillError::Decode(e) => {
                    ConnectError::Unsupported(format!("{url} sent a malformed handshake: {e}"))
                }
            })?;
            if !filled {
                return Err(ConnectError::Unreachable(format!(
                    "{url} closed the stream before sending a handshake"
                )));
            }
        }
        match sub.pending.first() {
            Some(Frame::Handshake { .. }) => {
                let handshake = sub.pending.remove(0);
                sub.subscriber
                    .apply(vec![handshake])
                    .map_err(ConnectError::Unsupported)?;
            }
            Some(other) => {
                return Err(ConnectError::Unsupported(format!(
                    "{url} opened its stream with a {} frame instead of a handshake",
                    frame_kind(other)
                )))
            }
            None => unreachable!("the loop above exits with a frame pending"),
        }

        Ok(sub)
    }

    /// The next complete interval, or `None` when the agent closed the stream.
    ///
    /// An interval is every frame up to and including a `Rows`, which is what
    /// the producer emits last. Waiting for the `Rows` is what makes the batch
    /// handed to [`StreamSubscriber::apply`] a whole interval rather than a
    /// fragment.
    pub(crate) async fn next_interval(&mut self) -> Result<Option<Applied>, String> {
        loop {
            if let Some(at) = self
                .pending
                .iter()
                .position(|f| matches!(f, Frame::Rows { .. }))
            {
                let batch: Vec<Frame> = self.pending.drain(..=at).collect();
                return self.subscriber.apply(batch).map(Some);
            }
            if !self.fill().await.map_err(FillError::into_message)? {
                return Ok(None);
            }
        }
    }

    /// Read one chunk off the connection and decode what it completes into
    /// `pending`. `Ok(false)` means the agent closed the stream cleanly.
    async fn fill(&mut self) -> Result<bool, FillError> {
        let chunk = self
            .response
            .chunk()
            .await
            .map_err(|e| FillError::Transport(format!("replication stream failed: {e}")))?;
        let Some(chunk) = chunk else {
            // The agent closed. Bytes still held back mean it closed
            // mid-frame, which is worth saying — a clean end leaves none.
            if self.decoder.pending() > 0 {
                return Err(FillError::Transport(format!(
                    "the agent closed the stream mid-frame, with {} byte(s) unread",
                    self.decoder.pending()
                )));
            }
            return Ok(false);
        };
        self.pending
            .extend(self.decoder.push(&chunk).map_err(FillError::Decode)?);
        Ok(true)
    }

    /// How often this agent can have anything new to send — its snapshot
    /// TTL, as it reported it. An interval shorter than this is served, but
    /// most of its frames are empty, and a recording stamped with that
    /// interval would claim a resolution the data does not have.
    pub(crate) fn update_floor(&self) -> Option<Duration> {
        self.update_floor
    }

    /// The source this subscription is carrying, once its handshake has been
    /// applied.
    pub(crate) fn source(&self) -> Option<&Source> {
        self.subscriber.source()
    }

    /// The layout the agent serves this subscription.
    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }
}

/// What a [`pump`] reports to the recording loop.
#[derive(Debug)]
pub(crate) enum StreamEvent {
    /// One interval, applied.
    Interval(Applied),
    /// The connection ended — the agent closed it, or it failed — and the pump
    /// is reconnecting. Rows between here and the next `Connected` are lost,
    /// which the reconnecting subscription's `gap` will not show (it counts
    /// from its own first frame), so this is the record of it.
    Dropped(String),
    /// A fresh connection is up, with the source its handshake named. The
    /// loop compares it with the source the recording opened on: a different
    /// uuid is a restarted agent, and every cumulative counter reset with it.
    Connected(Source),
    /// The agent answered a reconnect and cannot serve the stream. Retrying
    /// will not change that, so the pump has stopped.
    Refused(String),
}

/// Drive one subscription for the life of the run.
///
/// Holds the connection so the recording loop does not have to: the loop's
/// job is to commit whatever arrived each tick, and a loop that also awaited
/// each endpoint's next frame would stall every other endpoint's commit on the
/// slowest connection. Each interval is sent as it completes; the channel is
/// bounded, so a loop that stops draining pushes back on the socket rather
/// than on memory.
///
/// A dropped connection is reconnected after `interval` — the cadence the
/// scrape path re-probes an unreachable endpoint at — with a floor of one
/// second so a short interval against a dead host is not a tight loop. Exits
/// when the loop has gone away (the send fails) or the agent refuses.
///
/// `timeout` bounds every wait on the socket. The agent sends a frame every
/// interval, empty when nothing is new, and says so is the keepalive: a
/// connection that produces nothing for longer than that is dead whatever the
/// kernel thinks, since a peer that vanished without a RST — a firewall that
/// dropped the state, a proxy that stopped forwarding — never closes it. The
/// scrape path bounds every scrape the same way; without this the stream had
/// no equivalent, and a silent connection was a recording that ended early
/// with no warning.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn pump(
    idx: usize,
    mut sub: Subscription,
    client: reqwest::Client,
    base: reqwest::Url,
    interval: Duration,
    layout: Layout,
    timeout: Duration,
    tx: tokio::sync::mpsc::Sender<(usize, StreamEvent)>,
) {
    let retry = interval.max(Duration::from_secs(1));
    loop {
        let outcome = match tokio::time::timeout(timeout, sub.next_interval()).await {
            Ok(Ok(Some(applied))) => {
                if tx
                    .send((idx, StreamEvent::Interval(applied)))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Ok(Ok(None)) => "the agent closed the stream".to_string(),
            Ok(Err(e)) => e,
            Err(_) => format!(
                "no frame arrived within {}; the connection is dead",
                humantime::format_duration(timeout)
            ),
        };
        if tx.send((idx, StreamEvent::Dropped(outcome))).await.is_err() {
            return;
        }
        sub = loop {
            tokio::time::sleep(retry).await;
            let connected = tokio::time::timeout(
                timeout,
                Subscription::connect(&client, &base, interval, layout),
            )
            .await;
            match connected {
                Ok(Ok(sub)) => break sub,
                // A connect that hangs is an outage like any other.
                Err(_) | Ok(Err(ConnectError::Unreachable(_))) => continue,
                Ok(Err(ConnectError::Unsupported(e))) => {
                    let _ = tx.send((idx, StreamEvent::Refused(e))).await;
                    return;
                }
            }
        };
        let source = sub
            .source()
            .cloned()
            .expect("connect returns only once the handshake has been applied");
        if tx
            .send((idx, StreamEvent::Connected(source)))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// A pass's decoded groups as the V3 snapshot a `.rez` writer ingests.
///
/// Only a wide-layout subscription's groups make one: a long row has no
/// member list to rebuild a group's schema from, and is refused.
pub(crate) fn wide_snapshot(
    groups: Vec<metriken_archive::StreamedGroup>,
    ts: i64,
    wall_offset: i64,
) -> Result<metriken_exposition::Snapshot, String> {
    use metriken_archive::StreamedGroup;
    use metriken_exposition::{Snapshot, SnapshotV3};
    let groups = groups
        .into_iter()
        .map(|g| match g {
            StreamedGroup::Wide(g) => Ok(g),
            StreamedGroup::Long { name, .. } | StreamedGroup::Occupants { table: name, .. } => {
                Err(format!(
                    "stream {name} arrived in the long layout, which a .rez archive cannot record"
                ))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    let wall = u64::try_from(ts.saturating_add(wall_offset)).unwrap_or(0);
    Ok(Snapshot::V3(SnapshotV3 {
        systemtime: std::time::UNIX_EPOCH + Duration::from_nanos(wall),
        // The stream carries no pass duration.
        duration: Duration::ZERO,
        metadata: Default::default(),
        groups,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const STREAM: &str = "cpu_usage/cpu_usage_task";

    fn handshake() -> Frame {
        Frame::Handshake {
            source: 0,
            uuid: Some("epoch-1".to_string()),
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: 1_700_000_000_000_000_000,
            complete: false,
        }
    }

    fn rows_frame(seq: u64, n: usize) -> Frame {
        Frame::Rows {
            source: 0,
            seq,
            index_state: dendro::replicate::NO_INDEX_STATE,
            rows: (0..n)
                .map(|i| WalRow {
                    stream: STREAM.to_string(),
                    ts: 1_000 + i as i64,
                    wall_offset: 0,
                    row: vec![0x93, 0x01],
                })
                .collect(),
        }
    }

    fn encoded_stream(frames: &[Frame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        dendro::replicate::wire::write_preamble(&mut bytes).unwrap();
        for f in frames {
            dendro::replicate::wire::encode_frame(f, &mut bytes).unwrap();
        }
        bytes
    }

    /// The case the decoder exists for: chunk boundaries that fall wherever
    /// the network put them. Feeding a stream one byte at a time is the
    /// cruellest split available and must produce exactly the frames that went
    /// in, in order.
    #[test]
    fn frames_survive_being_split_at_every_byte() {
        let sent = vec![handshake(), rows_frame(1, 2), rows_frame(2, 0)];
        let bytes = encoded_stream(&sent);

        let mut decoder = FrameDecoder::new();
        let mut got = Vec::new();
        for b in &bytes {
            got.extend(decoder.push(&[*b]).unwrap());
        }
        assert_eq!(got, sent);
        assert_eq!(decoder.pending(), 0, "nothing held back at the end");
    }

    /// And the other extreme: everything in one chunk.
    #[test]
    fn frames_survive_arriving_all_at_once() {
        let sent = vec![handshake(), rows_frame(1, 1)];
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.push(&encoded_stream(&sent)).unwrap(), sent);
    }

    /// A partial frame is not an error and not a frame: it is held until the
    /// rest arrives. A decoder that errored here would fail on every stream
    /// whose chunks did not happen to align.
    #[test]
    fn a_partial_frame_yields_nothing_and_is_not_an_error() {
        let bytes = encoded_stream(&[handshake()]);
        let mut decoder = FrameDecoder::new();
        let half = bytes.len() / 2;
        assert!(decoder.push(&bytes[..half]).unwrap().is_empty());
        assert!(decoder.pending() > 0);
        assert_eq!(decoder.push(&bytes[half..]).unwrap().len(), 1);
    }

    /// Wrong magic is named for what it is. The likely cause is an endpoint
    /// serving the older msgpack stream, and a decoder that limped on would
    /// report malformed frames forever instead of the one fact that explains
    /// them.
    #[test]
    fn a_stream_that_is_not_a_replication_stream_says_so() {
        let mut decoder = FrameDecoder::new();
        let err = decoder
            .push(b"\x93\x01\x02 this is msgpack, not frames")
            .expect_err("must refuse");
        assert!(err.contains("not a replication stream"), "{err}");
    }

    /// A length prefix past the limit is refused rather than allocated. A
    /// corrupt or hostile four-byte length is otherwise an allocation of up to
    /// 4 GiB, which is an out-of-memory rather than an error.
    #[test]
    fn an_absurd_frame_length_is_refused_rather_than_allocated() {
        let mut bytes = Vec::new();
        dendro::replicate::wire::write_preamble(&mut bytes).unwrap();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());

        let mut decoder = FrameDecoder::new();
        let err = decoder.push(&bytes).expect_err("must refuse");
        assert!(err.contains("past the"), "{err}");
    }

    /// The ordinary interval: a handshake, then rows.
    #[test]
    fn an_interval_keeps_its_rows() {
        let mut sub = StreamSubscriber::new();
        let applied = sub.apply(vec![handshake(), rows_frame(7, 3)]).unwrap();

        assert_eq!(applied.rows.len(), 3);
        assert_eq!(applied.seq, 7);
        assert_eq!(
            sub.source().unwrap().clock_anchor_wall_ns,
            1_700_000_000_000_000_000,
            "the producer's anchor is kept: its rows are on that timeline"
        );
    }

    /// An agent before 6.0 sends index frames and stamps its rows with its own
    /// index state. The frames are dropped and the rows kept whatever state
    /// they name: identity reaches the archive through the schemas.
    #[test]
    fn an_older_agents_index_frames_are_ignored() {
        let mut sub = StreamSubscriber::new();
        let applied = sub
            .apply(vec![
                handshake(),
                Frame::Index {
                    source: 0,
                    stream: STREAM.to_string(),
                    ts: 1_000,
                    kind: dendro::replicate::IndexKind::Full,
                    state: (0xdead, 0xbeef),
                    blob: vec![0x93, 0x01],
                },
                Frame::Rows {
                    source: 0,
                    seq: 1,
                    index_state: (0xdead, 0xbeef),
                    rows: vec![WalRow {
                        stream: STREAM.to_string(),
                        ts: 1_000,
                        wall_offset: 0,
                        row: vec![0x93, 0x01],
                    }],
                },
            ])
            .unwrap();
        assert_eq!(applied.rows.len(), 1);
    }

    /// A gap means intervals produced no frame at all — the subscriber was
    /// starved past its boundary, or frames were lost. It is reported rather
    /// than smoothed over.
    #[test]
    fn a_jump_in_seq_is_reported_as_a_gap() {
        let mut sub = StreamSubscriber::new();
        sub.apply(vec![handshake(), rows_frame(1, 1)]).unwrap();

        let contiguous = sub.apply(vec![rows_frame(2, 1)]).unwrap();
        assert!(!contiguous.gap);

        let jumped = sub.apply(vec![rows_frame(9, 1)]).unwrap();
        assert!(
            jumped.gap,
            "seq 2 -> 9 is six intervals that produced nothing"
        );
    }

    /// A producer restart is a new timeline, not more of this one. Appending
    /// its rows to the same recording would interleave two monotonic series
    /// under one identity.
    #[test]
    fn a_source_change_mid_connection_is_refused() {
        let mut sub = StreamSubscriber::new();
        sub.apply(vec![handshake()]).unwrap();

        let Frame::Handshake {
            source,
            labels,
            metadata,
            clock_anchor_wall_ns,
            complete,
            ..
        } = handshake()
        else {
            unreachable!()
        };
        let restarted = Frame::Handshake {
            source,
            uuid: Some("epoch-2".to_string()),
            labels,
            metadata,
            clock_anchor_wall_ns,
            complete,
        };
        let err = sub.apply(vec![restarted]).expect_err("must refuse");
        assert!(err.contains("different timeline"), "{err}");
    }

    /// A frame only an archive publisher sends is an error rather than
    /// something to guess at: a live agent has no archive, so a stream
    /// producing one is not this protocol.
    #[test]
    fn a_segment_frame_on_an_agent_stream_is_refused() {
        let mut sub = StreamSubscriber::new();
        let err = sub
            .apply(vec![
                handshake(),
                Frame::Segment {
                    source: 0,
                    stream: STREAM.to_string(),
                    meta: dendro::archive::SegmentMeta {
                        rows: 0,
                        first_ts: 0,
                        last_ts: 0,
                    },
                    bytes: Vec::new(),
                    caller_index: None,
                },
            ])
            .expect_err("must refuse");
        assert!(err.contains("segment"), "{err}");
    }

    /// A payload the producer would send: an encoded `WalGroupRow`, with the
    /// schema inside on its first mention and absent after.
    fn payload(n: usize, with_schema: bool, window_end: u64) -> Vec<u8> {
        let schema = crate::recorder::schema::GroupSchema {
            counters: (0..n)
                .map(|i| crate::recorder::schema::MetricDesc {
                    name: format!("0x{i}"),
                    metadata: [("metric".to_string(), "cpu_usage_user".to_string())]
                        .into_iter()
                        .collect(),
                })
                .collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        crate::recorder::wal::encode_wal_group_row(&crate::recorder::wal::WalGroupRow {
            schema_hash: schema.hash(),
            schema: with_schema.then_some(schema),
            window: Some((window_end - 500, window_end)),
            counters: (0..n).map(|i| Some(i as u64)).collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
        .unwrap()
    }

    /// The groups of one row, as a connection's decoder reads it.
    fn decode(
        decoder: &mut metriken_archive::StreamDecoder,
        row: Vec<u8>,
    ) -> Vec<metriken_exposition::GroupSnapshot> {
        decoder
            .decode([wal_row("fake/ops", 1_000, 0, row)])
            .unwrap()
            .into_iter()
            .map(|g| match g {
                metriken_archive::StreamedGroup::Wide(g) => g,
                _ => panic!("a wide group"),
            })
            .collect()
    }

    /// The schema rides only on the row where it arrived, so the writer
    /// validates it once per change; rows after it resolve by hash.
    #[test]
    fn a_streamed_schema_is_attached_where_it_arrived() {
        let mut decoder = metriken_archive::StreamDecoder::new();
        let first = decode(&mut decoder, payload(2, true, 2_000)).remove(0);
        assert_eq!(first.schema.as_ref().map(|s| s.counters.len()), Some(2));
        assert_eq!(first.counters, vec![Some(0), Some(1)]);
        let next = decode(&mut decoder, payload(2, false, 3_000)).remove(0);
        assert!(next.schema.is_none(), "known by hash, not re-sent");
        assert_eq!(next.schema_hash, first.schema_hash);
        assert_eq!(decoder.unresolved, 0);
    }

    /// A row naming a schema this connection never sent is skipped and
    /// counted, not decoded against the wrong members.
    #[test]
    fn a_streamed_row_with_an_unsent_schema_is_skipped() {
        let mut decoder = metriken_archive::StreamDecoder::new();
        decode(&mut decoder, payload(2, true, 2_000));
        assert!(decode(&mut decoder, payload(3, false, 3_000)).is_empty());
        assert_eq!(decoder.unresolved, 1);
    }

    /// A `.rez` archive records snapshots: a pass of wide groups becomes one
    /// at the pass's wall clock, and a long row is refused by name.
    #[test]
    fn only_a_wide_pass_rebuilds_a_snapshot() {
        let mut decoder = metriken_archive::StreamDecoder::new();
        let groups = decoder
            .decode([wal_row(STREAM, 5_000, 7, payload(2, true, 5_000))])
            .unwrap();
        let metriken_exposition::Snapshot::V3(v3) = wide_snapshot(groups, 5_000, 7).unwrap() else {
            panic!("a V3 snapshot");
        };
        assert_eq!(v3.groups.len(), 1);
        assert_eq!(
            v3.systemtime,
            std::time::UNIX_EPOCH + Duration::from_nanos(5_007)
        );
        let long = vec![metriken_archive::StreamedGroup::Occupants {
            table: STREAM.to_string(),
            occupants: Vec::new(),
        }];
        let err = wide_snapshot(long, 5_000, 0).expect_err("a long pass");
        assert!(err.contains(STREAM) && err.contains("long layout"), "{err}");
    }

    fn wal_row(stream: &str, ts: i64, wall_offset: i64, row: Vec<u8>) -> WalRow {
        WalRow {
            stream: stream.to_string(),
            ts,
            wall_offset,
            row,
        }
    }

    /// The ordinary interval: every row shares the pass's stamp, so the
    /// writer gets ONE pass at that stamp.
    #[test]
    fn an_interval_becomes_one_pass_at_the_producers_stamp() {
        let applied = Applied {
            seq: 3,
            rows: vec![
                wal_row(STREAM, 5_000, 7, payload(2, true, 5_000)),
                wal_row("b/two", 5_000, 7, payload(1, false, 5_000)),
            ],
            gap: false,
        };
        let passes = applied.for_writer().unwrap();

        assert_eq!(passes.len(), 1, "one pass, not one per row");
        let pass = &passes[0];
        assert_eq!((pass.ts, pass.wall_offset), (5_000, 7));
        assert_eq!(pass.rows.len(), 2);
        assert_eq!(pass.rows[0].stream, STREAM);
        assert_eq!(pass.rows[1].stream, "b/two", "in the order they arrived");
    }

    /// A frame carrying two stamps is two passes: a relay that batched them
    /// must not have the second pass's rows attributed to the first's clock.
    #[test]
    fn rows_at_two_stamps_become_two_passes() {
        let applied = Applied {
            rows: vec![
                wal_row(STREAM, 5_000, 0, payload(1, true, 5_000)),
                wal_row(STREAM, 6_000, 0, payload(1, false, 6_000)),
            ],
            ..Applied::default()
        };
        let passes = applied.for_writer().unwrap();
        assert_eq!(
            passes.iter().map(|p| p.ts).collect::<Vec<_>>(),
            vec![5_000, 6_000]
        );
        assert!(passes.iter().all(|p| p.rows.len() == 1));
    }

    /// Two rows at one `ts` are one pass whatever their wall offsets say,
    /// because one `ts` is one WAL key per stream: staged as two passes, a
    /// stream present in both would collide on it and kill the writer.
    #[test]
    fn rows_at_one_stamp_are_one_pass_whatever_their_wall_offsets() {
        let applied = Applied {
            rows: vec![
                wal_row(STREAM, 5_000, 0, payload(1, true, 5_000)),
                wal_row("b/two", 5_000, 9, payload(1, true, 5_000)),
            ],
            ..Applied::default()
        };
        let passes = applied.for_writer().unwrap();
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].rows.len(), 2);
        assert_eq!(passes[0].wall_offset, 0, "the first row's stands");
    }

    /// The archive's `ts` is unsigned. A producer stamping before the epoch
    /// is refused, not clamped: a clamp would put its rows at 1970 and a
    /// consumer would draw them there.
    #[test]
    fn a_stamp_before_the_epoch_is_refused() {
        let rows = Applied {
            rows: vec![wal_row(STREAM, -1, 0, payload(1, true, 5_000))],
            ..Applied::default()
        };
        let err = rows.for_writer().expect_err("a negative row stamp");
        assert!(
            err.contains(STREAM) && err.contains("before the epoch"),
            "{err}"
        );
    }

    /// A payload that will not decode fails the interval and names the
    /// stream. The producer is this binary, so the cause is a version
    /// mismatch, and a subscriber that quietly dropped the row would record a
    /// gap nothing explains.
    #[test]
    fn an_undecodable_payload_fails_the_interval_by_name() {
        let applied = Applied {
            rows: vec![wal_row(STREAM, 5_000, 0, vec![0x93, 0x01])],
            ..Applied::default()
        };
        let pass = applied.for_writer().unwrap().remove(0);
        let err = metriken_archive::StreamDecoder::new()
            .decode(pass.rows)
            .err()
            .expect("must refuse");
        assert!(err.contains(STREAM), "{err}");
    }
}
