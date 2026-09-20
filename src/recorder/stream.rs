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

use dendro::archive::WalRow;
use dendro::replicate::Frame;

/// What one interval's frames amounted to.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The interval index the producer stamped. Not a frame count: a gap says
    /// an interval the subscriber asked for produced no reading.
    pub seq: u64,
    /// Rows whose index state resolved, ready for the writer.
    pub rows: Vec<WalRow>,
    /// Index entries, by stream, for `caller_rows`. The blob is opaque here
    /// exactly as it is in the archive.
    pub entries: Vec<(String, Vec<u8>)>,
    /// Rows dropped because the state they named is not the state this
    /// subscriber holds.
    pub rows_skipped: usize,
    /// Whether `seq` jumped, meaning intervals produced no frame at all.
    pub gap: bool,
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
    /// rather than just store it.
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
                    self.index
                        .apply(&stream, &entry)
                        .map_err(|e| format!("index entry on `{stream}` at {ts}: {e}"))?;
                    out.entries.push((stream, blob));
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
}
