//! Consuming an agent's replication stream.
//!
//! The agent produces `Frame::{Handshake, Index, Rows}` over HTTP
//! (`/metrics/stream`); this turns them back into the rows and index entries a
//! `.rez` recording holds.
//!
//! # Why not dendro's `Subscriber`
//!
//! dendro ships one, and it implements exactly these rules. It also owns a
//! dendro `Writer` and produces a dendro archive — a different container from
//! `.rez`, which is what `RezReader`, both viewers, MCP and `parquet_tools`
//! read. Until that migration happens a subscriber has to write `.rez`, so the
//! rules are implemented here against [`SourceIndex`], which is the same type
//! the producer accumulates with. One type, two callers, no second
//! implementation of the state hash to keep in agreement.
//!
//! # The rule that matters
//!
//! A rows frame names the index state it was built against. A subscriber whose
//! accumulated state differs **skips those rows** and says so: attributing one
//! task's numbers to another is worse than a gap. That is rule 10, and
//! [`Applied::rows_skipped`] is how a caller finds out it happened.

use crate::recorder::index::{IndexEntry, IndexState, SourceIndex};
use crate::recorder::rez_sqlite::IndexEntries;
use crate::recorder::wire::{AgentRow, AgentRows};

use dendro::archive::WalRow;
use dendro::replicate::Frame;
use std::time::Duration;

/// What one interval's frames amounted to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The interval index the producer stamped. Not a frame count: a gap says
    /// an interval the subscriber asked for produced no reading.
    pub seq: u64,
    /// Rows whose index state resolved, ready for the writer.
    pub rows: Vec<WalRow>,
    /// Index entries for `caller_rows`: `(stream, ts, blob)`. The blob is
    /// opaque here exactly as it is in the archive; `ts` is the producer's
    /// stamp on the frame, which is the stamp of the rows the entry describes.
    pub entries: Vec<(String, i64, Vec<u8>)>,
    /// Rows dropped because the state they named is not the state this
    /// subscriber holds.
    pub rows_skipped: usize,
    /// Whether `seq` jumped, meaning intervals produced no frame at all.
    pub gap: bool,
}

/// One interval, in the shape the writer stages.
///
/// `rows` is grouped by the producer's stamp: one `AgentRows` per distinct
/// `(ts, wall_offset)`, which for a well-formed interval is exactly one — the
/// producer stamps a whole pass once. Grouping rather than assuming keeps a
/// relay that batches several passes into one frame from landing them all on
/// the first pass's stamp.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Interval {
    pub rows: Vec<AgentRows>,
    pub index_entries: IndexEntries,
}

impl Applied {
    /// Turn the rows and entries into what `StreamRecorderV3::stage_rows`
    /// and `RecordingWriter::wal_with_index` take.
    ///
    /// Each payload is decoded once, here, to rebuild the envelope the row
    /// endpoint would have sent — see [`AgentRow::from_payload`] for why the
    /// stream is fed through the same staging path rather than a new one.
    /// A payload that will not decode fails the interval rather than being
    /// dropped: the producer is this binary, so an undecodable row is a
    /// version mismatch, and a stream that quietly thinned itself would
    /// record a gap nothing explains.
    ///
    /// A negative stamp is refused for the reason `snapshot_producer_stamp`
    /// refuses one: the archive's `ts` is unsigned, and a producer that sent
    /// one is not one to guess for.
    pub(crate) fn for_writer(self) -> Result<Interval, String> {
        let mut by_stamp: std::collections::BTreeMap<(i64, i64), Vec<AgentRow>> =
            std::collections::BTreeMap::new();
        for row in self.rows {
            if row.ts < 0 {
                return Err(format!(
                    "stream {} stamped a row at {} ns, before the epoch",
                    row.stream, row.ts
                ));
            }
            by_stamp
                .entry((row.ts, row.wall_offset))
                .or_default()
                .push(AgentRow::from_payload(row.stream, row.row)?);
        }
        let rows = by_stamp
            .into_iter()
            .map(|((ts, wall_offset), rows)| AgentRows {
                // `ts + wall_offset` is the wall clock at the pass by the
                // producer's own definition; the stream carries no pass
                // duration, so that field is what a windowless reading gets.
                wall_ns: ts.saturating_add(wall_offset).max(0) as u64,
                duration_ns: 0,
                ts,
                wall_offset,
                rows,
            })
            .collect();

        let mut index_entries: IndexEntries = Vec::new();
        for (stream, ts, blob) in self.entries {
            let ts = u64::try_from(ts).map_err(|_| {
                format!("index entry on `{stream}` stamped at {ts} ns, before the epoch")
            })?;
            match index_entries.iter_mut().find(|(s, _)| *s == stream) {
                Some((_, rows)) => rows.push((ts, blob)),
                None => index_entries.push((stream, vec![(ts, blob)])),
            }
        }
        Ok(Interval {
            rows,
            index_entries,
        })
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
    index: SourceIndex,
    source: Option<Source>,
    last_seq: Option<u64>,
    skipped_total: usize,
}

impl StreamSubscriber {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The source this stream is carrying, once its handshake has arrived.
    pub(crate) fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// Rows dropped over this connection's life, for the caller to report.
    pub(crate) fn skipped_total(&self) -> usize {
        self.skipped_total
    }

    /// What a slot currently means, for a caller that wants to read identity
    /// rather than just store it. The recorder stores; the tests read.
    #[cfg(test)]
    pub(crate) fn index(&self) -> &SourceIndex {
        &self.index
    }

    /// Fold one interval's frames in.
    ///
    /// Takes the frames of a whole interval rather than one at a time because
    /// the order within an interval is the covenant: index entries precede the
    /// rows that reference them, so a row can never name identity that has not
    /// been applied yet. A caller feeding them singly could not preserve that
    /// without reimplementing it.
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

                Frame::Index {
                    stream, blob, ts, ..
                } => {
                    // Decoded to apply, stored as the bytes that arrived. The
                    // archive keeps the producer's encoding rather than a
                    // re-encoding of our decode, so a consumer reading it back
                    // sees what was sent.
                    let entry = IndexEntry::decode(&blob)
                        .map_err(|e| format!("undecodable index entry on `{stream}`: {e}"))?;
                    // Applied without comparing, because a complete
                    // restatement is several entries carrying one state
                    // between them — see `SourceIndex::apply_unchecked`. The
                    // comparison happens once, against the rows below.
                    self.index
                        .apply_unchecked(&stream, &entry)
                        .map_err(|e| format!("index entry on `{stream}` at {ts}: {e}"))?;
                    out.entries.push((stream, ts, blob));
                }

                Frame::Rows {
                    seq,
                    index_state,
                    rows,
                    ..
                } => {
                    if let Some(last) = self.last_seq {
                        out.gap = seq > last.saturating_add(1);
                    }
                    self.last_seq = Some(seq);
                    out.seq = seq;

                    if !self.resolves(index_state) {
                        // Rule 10. Counted, not silently dropped: a caller that
                        // cannot tell a skip from a quiet interval cannot tell
                        // a broken stream from an idle one.
                        out.rows_skipped += rows.len();
                        self.skipped_total += rows.len();
                        continue;
                    }
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

    /// Whether rows naming `state` can be attributed.
    fn resolves(&self, state: IndexState) -> bool {
        if state == dendro::replicate::NO_INDEX_STATE {
            // The producer keeps no index. Always resolvable, and rezolus's
            // own producer never sends it — but a relay that does must not
            // have every row dropped waiting for a `Full` that is not coming.
            return true;
        }
        // The accumulated state hashing to what the rows claim is the whole
        // test. There is deliberately no "have I seen a Full yet" guard
        // alongside it, and the difference matters in one case: a producer
        // whose groups have no slots sends a full state containing no entries,
        // so no index frame arrives at all, and rows then name the empty
        // state. A `seen_full` guard would skip every row of such a stream for
        // the life of the connection, while the hash says — correctly — that
        // an empty set is what both sides hold.
        //
        // It costs nothing elsewhere: a subscriber that joined mid-stream
        // holds an empty index, and a producer with slots stamps a non-empty
        // state, so the hashes differ and the rows are skipped exactly as they
        // should be. dendro's own subscriber carries the extra guard because
        // an archive publisher may start indexing mid-stream; rezolus's
        // producer sends each stream's first entry as a `Full`, so that cannot
        // arise here.
        state == self.index.state()
    }
}

/// Names a frame for an error message.
///
/// Deliberately exhaustive, with no catch-all arm: a variant added to dendro's
/// `Frame` should stop this compiling, so somebody decides what a subscriber
/// does with it rather than it being reported as "unknown" and moved past.
fn frame_kind(frame: &Frame) -> &'static str {
    match frame {
        Frame::Handshake { .. } => "handshake",
        Frame::Index { .. } => "index",
        Frame::Rows { .. } => "rows",
        Frame::Segment { .. } => "segment",
        Frame::ClockOffset { .. } => "clock offset",
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
                    "replication protocol version {version}, but this build speaks {}",
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
/// loudly rather than falling back to scraping: `--stream` names a transport,
/// and a run that silently used another would put two endpoints of one A/B on
/// different transports.
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
    ) -> Result<Self, ConnectError> {
        let mut url = base.clone();
        url.set_path("/metrics/stream");
        url.set_query(Some(&format!(
            "interval={}",
            humantime::format_duration(interval)
        )));

        let response =
            client.get(url.clone()).send().await.map_err(|e| {
                ConnectError::Unreachable(format!("failed to subscribe to {url}: {e}"))
            })?;

        if !response.status().is_success() {
            // 409 is the agent saying it has no acquisition groups to stream —
            // a V2 agent. Worth distinguishing from a transport failure,
            // because retrying will never fix it.
            return Err(ConnectError::Unsupported(
                match response.status().as_u16() {
                    409 => format!(
                        "{url} cannot serve a replication stream: the agent reports no \
                     acquisition groups, which a V2 agent never has"
                    ),
                    404 => format!(
                        "{url} returned HTTP 404: this agent has no replication stream (it \
                     predates /metrics/stream)"
                    ),
                    code => format!("{url} returned HTTP {code}"),
                },
            ));
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
                "{url} serves `{content_type}`, not `{}` — this build speaks only the \
                 replication stream",
                crate::agent::REPLICATION_CONTENT_TYPE
            )));
        }

        let update_floor = response
            .headers()
            .get("x-rezolus-update-floor")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| humantime::parse_duration(v).ok());

        let mut sub = Self {
            response,
            decoder: FrameDecoder::new(),
            subscriber: StreamSubscriber::new(),
            pending: Vec::new(),
            update_floor,
        };

        // The handshake is the first frame by protocol. Anything else first
        // is a producer this subscriber was not written for, and a connection
        // that ends before it is a transport failure like any other.
        while sub.pending.is_empty() {
            if !sub.fill().await.map_err(ConnectError::Unreachable)? {
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
    /// the producer emits last — index entries first, then the rows that
    /// reference them. Waiting for the `Rows` is what makes the batch handed
    /// to [`StreamSubscriber::apply`] a whole interval rather than a fragment,
    /// which that method requires.
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
            if !self.fill().await? {
                return Ok(None);
            }
        }
    }

    /// Read one chunk off the connection and decode what it completes into
    /// `pending`. `Ok(false)` means the agent closed the stream cleanly.
    async fn fill(&mut self) -> Result<bool, String> {
        let chunk = self
            .response
            .chunk()
            .await
            .map_err(|e| format!("replication stream failed: {e}"))?;
        let Some(chunk) = chunk else {
            // The agent closed. Bytes still held back mean it closed
            // mid-frame, which is worth saying — a clean end leaves none.
            if self.decoder.pending() > 0 {
                return Err(format!(
                    "the agent closed the stream mid-frame, with {} byte(s) unread",
                    self.decoder.pending()
                ));
            }
            return Ok(false);
        };
        self.pending.extend(self.decoder.push(&chunk)?);
        Ok(true)
    }

    /// Rows dropped over this subscription's life.
    pub(crate) fn skipped_total(&self) -> usize {
        self.subscriber.skipped_total()
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
pub(crate) async fn pump(
    idx: usize,
    mut sub: Subscription,
    client: reqwest::Client,
    base: reqwest::Url,
    interval: Duration,
    tx: tokio::sync::mpsc::Sender<(usize, StreamEvent)>,
) {
    let retry = interval.max(Duration::from_secs(1));
    loop {
        let outcome = match sub.next_interval().await {
            Ok(Some(applied)) => {
                if tx
                    .send((idx, StreamEvent::Interval(applied)))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Ok(None) => "the agent closed the stream".to_string(),
            Err(e) => e,
        };
        // The connection's skip count goes with its obituary: it is the one
        // number that says whether the rows it did deliver were attributable.
        let outcome = match sub.skipped_total() {
            0 => outcome,
            n => format!("{outcome}; {n} row(s) over the connection named an unheld index state"),
        };
        if tx.send((idx, StreamEvent::Dropped(outcome))).await.is_err() {
            return;
        }
        sub = loop {
            tokio::time::sleep(retry).await;
            match Subscription::connect(&client, &base, interval).await {
                Ok(sub) => break sub,
                Err(ConnectError::Unreachable(_)) => continue,
                Err(ConnectError::Unsupported(e)) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::index::SourceIndex;
    use std::collections::BTreeMap;

    const STREAM: &str = "cpu_usage/cpu_usage_task";

    fn labels(comm: &str) -> BTreeMap<String, String> {
        [("comm".to_string(), comm.to_string())]
            .into_iter()
            .collect()
    }

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

    fn index_frame(stream: &str, entry: &IndexEntry) -> Frame {
        Frame::Index {
            source: 0,
            stream: stream.to_string(),
            ts: 1_000,
            kind: entry.kind.into(),
            state: entry.state,
            blob: entry.encode(),
        }
    }

    fn rows_frame(seq: u64, state: IndexState, n: usize) -> Frame {
        Frame::Rows {
            source: 0,
            seq,
            index_state: state,
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
        let sent = vec![
            handshake(),
            rows_frame(1, dendro::replicate::NO_INDEX_STATE, 2),
            rows_frame(2, dendro::replicate::NO_INDEX_STATE, 0),
        ];
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
        let sent = vec![
            handshake(),
            rows_frame(1, dendro::replicate::NO_INDEX_STATE, 1),
        ];
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

    /// The ordinary interval: identity first, then the rows that reference it.
    #[test]
    fn an_interval_applies_its_entries_and_keeps_its_rows() {
        let mut producer = SourceIndex::new();
        let entry = producer
            .observe(STREAM, vec![(0u32, labels("redis"))])
            .unwrap();

        let mut sub = StreamSubscriber::new();
        let applied = sub
            .apply(vec![
                handshake(),
                index_frame(STREAM, &entry),
                rows_frame(7, producer.state(), 3),
            ])
            .unwrap();

        assert_eq!(applied.rows.len(), 3);
        assert_eq!(applied.rows_skipped, 0);
        assert_eq!(applied.seq, 7);
        assert_eq!(applied.entries.len(), 1);
        assert_eq!(applied.entries[0].0, STREAM);
        assert_eq!(
            sub.index().stream(STREAM).unwrap().labels(0).unwrap()["comm"],
            "redis"
        );
        assert_eq!(
            sub.source().unwrap().clock_anchor_wall_ns,
            1_700_000_000_000_000_000,
            "the producer's anchor is kept: its rows are on that timeline"
        );
    }

    /// Rule 10. Rows naming a state this subscriber does not hold are dropped,
    /// because attributing one task's numbers to another is worse than a gap.
    #[test]
    fn rows_naming_an_unheld_state_are_skipped_and_counted() {
        let mut sub = StreamSubscriber::new();
        let mut producer = SourceIndex::new();
        let entry = producer
            .observe(STREAM, vec![(0u32, labels("redis"))])
            .unwrap();
        sub.apply(vec![handshake(), index_frame(STREAM, &entry)])
            .unwrap();

        // The producer moved on; this subscriber never saw the entry that did
        // it.
        let _lost = producer.observe(STREAM, vec![(0u32, labels("valkey"))]);

        let applied = sub.apply(vec![rows_frame(8, producer.state(), 4)]).unwrap();
        assert!(applied.rows.is_empty());
        assert_eq!(applied.rows_skipped, 4);
        assert_eq!(sub.skipped_total(), 4);
    }

    /// A complete restatement is several entries carrying ONE state between
    /// them — the state after all of them. Checking after each would refuse
    /// the first, which is what a connecting subscriber always receives.
    #[test]
    fn a_restatement_spread_over_several_entries_applies_whole() {
        let mut producer = SourceIndex::new();
        producer
            .observe("a/one", vec![(0u32, labels("x"))])
            .unwrap();
        producer
            .observe("b/two", vec![(0u32, labels("y"))])
            .unwrap();
        producer
            .observe("c/three", vec![(0u32, labels("z"))])
            .unwrap();

        let full = producer.full_entries();
        assert_eq!(full.len(), 3);
        assert!(
            full.iter().all(|(_, e)| e.state == full[0].1.state),
            "fixture: a restatement carries one state across its entries"
        );

        let mut frames = vec![handshake()];
        frames.extend(full.iter().map(|(s, e)| index_frame(s, e)));
        frames.push(rows_frame(1, producer.state(), 2));

        let mut sub = StreamSubscriber::new();
        let applied = sub.apply(frames).unwrap();
        assert_eq!(
            applied.rows_skipped, 0,
            "every entry applied, so the rows attribute"
        );
        assert_eq!(applied.rows.len(), 2);
        assert_eq!(sub.index().state(), producer.state());
    }

    /// And losing one of those entries still skips the rows, which is the
    /// property moving the check was not allowed to cost. The accumulated set
    /// hashes to something other than what the rows name, so they are not
    /// attributed.
    #[test]
    fn losing_one_entry_of_a_restatement_still_skips_the_rows() {
        let mut producer = SourceIndex::new();
        producer
            .observe("a/one", vec![(0u32, labels("x"))])
            .unwrap();
        producer
            .observe("b/two", vec![(0u32, labels("y"))])
            .unwrap();
        producer
            .observe("c/three", vec![(0u32, labels("z"))])
            .unwrap();

        let full = producer.full_entries();
        let mut frames = vec![handshake()];
        // The middle one never arrives.
        frames.extend(
            full.iter()
                .enumerate()
                .filter(|(i, _)| *i != 1)
                .map(|(_, (s, e))| index_frame(s, e)),
        );
        frames.push(rows_frame(1, producer.state(), 2));

        let mut sub = StreamSubscriber::new();
        let applied = sub.apply(frames).unwrap();
        assert_eq!(
            applied.rows_skipped, 2,
            "a stream missing from the accumulated set means the rows cannot be attributed"
        );
        assert!(applied.rows.is_empty());
    }

    /// A subscriber that joined mid-stream holds an empty index, and a
    /// producer with slots stamps a non-empty state, so its rows are not
    /// attributable and are skipped.
    #[test]
    fn rows_from_a_producer_whose_index_we_have_not_received_are_skipped() {
        let mut producer = SourceIndex::new();
        producer
            .observe(STREAM, vec![(0u32, labels("redis"))])
            .unwrap();

        let mut sub = StreamSubscriber::new();
        let applied = sub
            .apply(vec![handshake(), rows_frame(1, producer.state(), 2)])
            .unwrap();
        assert_eq!(applied.rows_skipped, 2);
    }

    /// But a producer whose groups have no slots sends a full state containing
    /// NO entries, so no index frame arrives and its rows name the empty state.
    /// Those are attributable — an empty set is what both sides hold — and a
    /// "have I seen an index entry yet" guard would skip every row of such a
    /// stream for the life of the connection.
    #[test]
    fn rows_from_a_producer_with_no_slots_at_all_are_kept() {
        let empty = SourceIndex::new();
        assert!(
            empty.full_entries().is_empty(),
            "fixture: a producer with no streams sends no index frames"
        );

        let mut sub = StreamSubscriber::new();
        let applied = sub
            .apply(vec![handshake(), rows_frame(1, empty.state(), 2)])
            .unwrap();
        assert_eq!(applied.rows_skipped, 0);
        assert_eq!(applied.rows.len(), 2);
    }

    /// A publisher that keeps no secondary index says so, and its rows are
    /// always resolvable. rezolus's own producer never sends this, but a relay
    /// might, and waiting for a `Full` that is not coming would drop every row
    /// for the life of the connection.
    #[test]
    fn rows_built_against_no_index_at_all_are_kept() {
        let mut sub = StreamSubscriber::new();
        let applied = sub
            .apply(vec![
                handshake(),
                rows_frame(1, dendro::replicate::NO_INDEX_STATE, 2),
            ])
            .unwrap();
        assert_eq!(applied.rows.len(), 2);
        assert_eq!(applied.rows_skipped, 0);
    }

    /// A gap means intervals produced no frame at all — the subscriber was
    /// starved past its boundary, or frames were lost. It is reported rather
    /// than smoothed over.
    #[test]
    fn a_jump_in_seq_is_reported_as_a_gap() {
        let mut sub = StreamSubscriber::new();
        sub.apply(vec![
            handshake(),
            rows_frame(1, dendro::replicate::NO_INDEX_STATE, 1),
        ])
        .unwrap();

        let contiguous = sub
            .apply(vec![rows_frame(2, dendro::replicate::NO_INDEX_STATE, 1)])
            .unwrap();
        assert!(!contiguous.gap);

        let jumped = sub
            .apply(vec![rows_frame(9, dendro::replicate::NO_INDEX_STATE, 1)])
            .unwrap();
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

    fn wal_row(stream: &str, ts: i64, wall_offset: i64, row: Vec<u8>) -> WalRow {
        WalRow {
            stream: stream.to_string(),
            ts,
            wall_offset,
            row,
        }
    }

    /// The ordinary interval: every row shares the pass's stamp, so the
    /// writer gets ONE `AgentRows` at that stamp, each row's envelope rebuilt
    /// from its payload, and the index entries keyed by stream at the same
    /// stamp.
    #[test]
    fn an_interval_becomes_one_pass_at_the_producers_stamp() {
        let applied = Applied {
            seq: 3,
            rows: vec![
                wal_row(STREAM, 5_000, 7, payload(2, true, 5_000)),
                wal_row("b/two", 5_000, 7, payload(1, false, 5_000)),
            ],
            entries: vec![
                (STREAM.to_string(), 5_000, vec![1, 2]),
                (STREAM.to_string(), 5_000, vec![3]),
                ("b/two".to_string(), 5_000, vec![4]),
            ],
            rows_skipped: 0,
            gap: false,
        };
        let interval = applied.for_writer().unwrap();

        assert_eq!(interval.rows.len(), 1, "one pass, not one per row");
        let pass = &interval.rows[0];
        assert_eq!((pass.ts, pass.wall_offset), (5_000, 7));
        assert_eq!(
            pass.wall_ns, 5_007,
            "the wall clock at the pass is ts + wall_offset, by definition"
        );
        assert_eq!(pass.rows.len(), 2);
        assert_eq!(pass.rows[0].stream, STREAM);
        assert_eq!(pass.rows[0].arity, (2, 0, 0));
        assert!(
            pass.rows[0].schema.is_some(),
            "the first mention's schema is lifted into the envelope"
        );
        assert_eq!(pass.rows[0].window, Some((4_500, 5_000)));
        assert_eq!(pass.rows[1].stream, "b/two");
        assert!(pass.rows[1].schema.is_none());

        // Grouped by stream, in the order the entries arrived within a
        // stream — `caller_rows` numbers same-ts entries by insertion.
        assert_eq!(
            interval.index_entries,
            vec![
                (
                    STREAM.to_string(),
                    vec![(5_000, vec![1, 2]), (5_000, vec![3])]
                ),
                ("b/two".to_string(), vec![(5_000, vec![4])]),
            ]
        );
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
        let interval = applied.for_writer().unwrap();
        assert_eq!(
            interval.rows.iter().map(|p| p.ts).collect::<Vec<_>>(),
            vec![5_000, 6_000]
        );
        assert!(interval.rows.iter().all(|p| p.rows.len() == 1));
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

        let entries = Applied {
            entries: vec![(STREAM.to_string(), -1, vec![1])],
            ..Applied::default()
        };
        let err = entries.for_writer().expect_err("a negative entry stamp");
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
        let err = applied.for_writer().expect_err("must refuse");
        assert!(err.contains(STREAM), "{err}");
    }
}
