//! A sampling pass as dendro replication frames.
//!
//! The agent is one dendro *source*: one clock domain, one identity, one
//! sequence of observations. This turns a tick into the frames that say so —
//! a [`Frame::Handshake`] once, a [`Frame::Index`] per group whose slots moved,
//! and one [`Frame::Rows`] per interval.
//!
//! # Time is the producer's
//!
//! The row endpoint stamps a WAL row's `ts` on the CONSUMER, deliberately: a
//! recording's timeline is then the recorder's, which no agent clock can
//! corrupt. Replication cannot work that way. dendro's FORMAT.md §5 says "one
//! source is one clock domain: rows from two producers with two clocks belong
//! in two sources", and the anchor that pins the timeline
//! (`clock_anchor_wall_ns`) is a handshake field — the producer's. If a
//! subscriber stamped, two recorders watching one agent would build two
//! timelines for the same source and could not be merged.
//!
//! So `ts` is anchored: `anchor_wall_ns + monotonic elapsed`, which has
//! wall-clock magnitude but advances monotonically, so rows stay strictly
//! increasing through an NTP step. `wall_offset` is wall minus `ts` at the
//! moment of the read, so `ts + wall_offset` recovers the real wall clock and
//! the divergence is visible rather than absorbed.
//!
//! The anchor comes from [`crate::agent::epoch`], where it is minted with the
//! `producer_epoch` and is therefore one per PROCESS. It used to be minted
//! here, per connection, which made the handshake advertise a different
//! `clock_anchor_wall_ns` for each subscriber while naming the same source
//! uuid: two subscribers to one agent were told the same source had two
//! timelines, differing by however far the wall clock moved between their
//! connections.
//!
//! # The schema travels inside the payload
//!
//! `/metrics/rows` carries a group's schema in the row ENVELOPE
//! ([`AgentRow::schema`]), which is what lets `build_rows` strip one the agent
//! has already sent. A [`WalRow`] has no envelope — `stream`, `ts`,
//! `wall_offset`, and an opaque payload. So the schema goes inside the payload,
//! which [`WalGroupRow`](crate::recorder::wal::WalGroupRow) already has a field
//! for and which the writer's schema ring already resolves. Same rule about
//! when to send it, a different place to put it.
//!
//! `arity` and `approx_bytes` do not survive the move, and are not missed: both
//! were envelope fields, and `approx_bytes` was the seal policy's size meter,
//! which under replication is the subscriber's business.

use std::collections::BTreeMap;

use dendro::archive::WalRow;
use dendro::replicate::{Frame, IndexState};

use crate::recorder::index::IndexEntry;
use crate::recorder::wire::AgentRows;

/// The ordinal the agent's own source takes. One agent is one source, so it is
/// always this; the field exists because a connection may carry several.
const SOURCE: u32 = 0;

/// Builds one subscription's frames.
///
/// One per subscription rather than one per agent, because the schema state it
/// keeps is "what THIS subscriber has been sent". Two subscribers that
/// connected at different times hold different schemas, and a producer shared
/// between them would tell the newer one a schema it had never seen was
/// already known.
pub(crate) struct FrameProducer {
    uuid: String,
    labels: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    /// Schemas this subscriber has been sent, by stream. The same rule
    /// `SnapshotBuilder::emitted_schemas` follows, kept per subscription
    /// because a stream is only self-describing to someone who has the schema
    /// its `schema_hash` names.
    sent_schemas: BTreeMap<String, (u64, u64)>,
    handshake_sent: bool,
}

impl FrameProducer {
    /// `epoch` is the agent's `producer_epoch`. It is the source uuid because
    /// the two mean the same thing — this producer until its counters restart
    /// — and using one value for both means a subscriber cannot see them
    /// disagree.
    pub(crate) fn new(
        epoch: String,
        labels: BTreeMap<String, String>,
        metadata: BTreeMap<String, String>,
    ) -> Self {
        Self {
            uuid: epoch,
            labels,
            metadata,
            sent_schemas: BTreeMap::new(),
            handshake_sent: false,
        }
    }

    /// The opening frame. Sent once per connection, before anything else.
    pub(crate) fn handshake(&mut self) -> Frame {
        self.handshake_sent = true;
        Frame::Handshake {
            source: SOURCE,
            uuid: Some(self.uuid.clone()),
            labels: self.labels.clone(),
            metadata: self.metadata.clone(),
            clock_anchor_wall_ns: crate::agent::epoch::clock_anchor_wall_ns(),
            // A live agent's source is open for as long as it is running.
            // `finish()` on the subscriber is what closes its copy, and it
            // must not be told the source ended when it has not.
            complete: false,
        }
    }

    /// One interval's frames: the index entries whose slots moved, then the
    /// rows they describe.
    ///
    /// The order is the covenant, not an implementation detail: within an
    /// interval metadata precedes data, so a row can never reference identity
    /// the subscriber has not already received. `index_state` is the state
    /// after every entry here has been applied, which is what the subscriber
    /// will hold by the time it reaches the rows.
    ///
    /// `seq` is the subscription's interval index rather than a count of
    /// frames. dendro documents `seq` as counting from zero per connection and
    /// uses it only for `seq > last + 1`, which an interval index satisfies
    /// identically — and it says the thing worth knowing when there is a gap:
    /// an interval the subscriber asked for produced no reading.
    pub(crate) fn interval(
        &mut self,
        rows: &AgentRows,
        entries: Vec<(String, IndexEntry)>,
        index_state: IndexState,
        seq: u64,
        mut keep: impl FnMut(&crate::recorder::wire::AgentRow) -> bool,
    ) -> Vec<Frame> {
        // The pass's own stamp, carried through rather than re-read here. A
        // frame emitted now can describe a pass that ran up to a TTL ago —
        // the snapshot is cached and a subscriber's interval does not drive
        // sampling — so stamping at emission would date every reading to the
        // moment it was sent.
        let ts = rows.ts;
        let wall_offset = rows.wall_offset;

        let mut frames = Vec::with_capacity(entries.len() + 1);
        for (stream, entry) in entries {
            frames.push(Frame::Index {
                source: SOURCE,
                stream,
                ts,
                kind: entry.kind.into(),
                state: entry.state,
                blob: entry.encode(),
            });
        }

        let wal_rows = rows
            .rows
            .iter()
            // Filtered BEFORE the schema decision below, not after. The other
            // way round would record a schema as taught on a row that was then
            // dropped, and the group would go on referencing a generation this
            // subscriber never received.
            .filter(|row| keep(row))
            .map(|row| {
                // The schema rides in the payload, so the payload has to be
                // rebuilt when it is included — the bytes `encode_group`
                // produced always say `None`.
                let payload = match self.sent_schemas.get(&row.stream) {
                    Some(hash) if *hash == row.schema_hash => row.row.clone(),
                    _ => {
                        self.sent_schemas
                            .insert(row.stream.clone(), row.schema_hash);
                        match &row.schema {
                            Some(schema) => with_schema(&row.row, schema.clone()),
                            // A producer that missed its own schema cache and
                            // still sent no schema has nothing to anchor with;
                            // pass the payload through and let the subscriber
                            // skip a row it cannot decode, which is what it
                            // does with an unresolvable hash anyway.
                            None => row.row.clone(),
                        }
                    }
                };
                WalRow {
                    stream: row.stream.clone(),
                    ts,
                    wall_offset,
                    row: payload,
                }
            })
            .collect();

        frames.push(Frame::Rows {
            source: SOURCE,
            seq,
            index_state,
            rows: wal_rows,
        });
        frames
    }

    /// An interval that produced no new reading: the empty frame, which is
    /// both the "your interval elapsed, nothing is new" signal and the
    /// keepalive.
    pub(crate) fn empty_interval(&self, index_state: IndexState, seq: u64) -> Frame {
        Frame::Rows {
            source: SOURCE,
            seq,
            index_state,
            rows: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn has_sent_handshake(&self) -> bool {
        self.handshake_sent
    }
}

/// The `Content-Type` of a replication stream.
///
/// Distinct from `wire::STREAM_CONTENT_TYPE`, which names a sequence of
/// msgpack `StreamFrame`s. This body is dendro's own framing — a preamble and
/// then length-prefixed frames — and a consumer that decoded one as the other
/// would read dendro's magic as msgpack.
pub(crate) const CONTENT_TYPE: &str = crate::agent::REPLICATION_CONTENT_TYPE;

/// Put `schema` into an encoded `WalGroupRow`, leaving everything else alone.
///
/// Decode-and-re-encode rather than a second construction from the snapshot:
/// the values are already encoded and re-deriving them would be a second
/// chance to disagree with what the row endpoint produces. On a payload that
/// will not decode, the original bytes are returned — the subscriber skips a
/// row it cannot read, and losing one row beats aborting a subscription.
fn with_schema(payload: &[u8], schema: crate::recorder::schema::GroupSchema) -> Vec<u8> {
    let Ok(mut row) = crate::recorder::wal::decode_wal_group_row(payload) else {
        return payload.to_vec();
    };
    row.schema = Some(schema);
    crate::recorder::wal::encode_wal_group_row(&row).unwrap_or_else(|_| payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::index::SourceIndex;
    use crate::recorder::wire::{AgentRow, AgentRows};
    use std::time::Duration;

    pub(super) const STREAM: &str = "cpu_usage/cpu_usage_task";

    pub(super) fn schema(n: usize) -> crate::recorder::schema::GroupSchema {
        crate::recorder::schema::GroupSchema {
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
        }
    }

    pub(super) fn payload(n: usize) -> Vec<u8> {
        crate::recorder::wal::encode_wal_group_row(&crate::recorder::wal::WalGroupRow {
            schema_hash: schema(n).hash(),
            schema: None,
            window: Some((1_000, 2_000)),
            counters: (0..n).map(|i| Some(i as u64)).collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
        .unwrap()
    }

    pub(super) fn rows(n: usize, wall_ns: u64) -> AgentRows {
        AgentRows {
            ts: wall_ns as i64,
            wall_offset: 0,
            wall_ns,
            duration_ns: 1_000,
            rows: vec![AgentRow {
                stream: STREAM.to_string(),
                window: Some((1_000, 2_000)),
                schema_hash: schema(n).hash(),
                schema: Some(schema(n)),
                arity: (n as u32, 0, 0),
                approx_bytes: 64,
                row: payload(n),
            }],
        }
    }

    pub(super) fn producer() -> FrameProducer {
        FrameProducer::new(
            "11111111-2222-4333-8444-555555555555".to_string(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            BTreeMap::new(),
        )
    }

    /// The schema is in the ENVELOPE on the row endpoint and has to be in the
    /// PAYLOAD here, because a `WalRow` has no envelope. A producer that moved
    /// the row across without moving the schema would hand the subscriber a
    /// payload naming a `schema_hash` it has no way to resolve.
    #[test]
    fn the_first_row_of_a_stream_carries_its_schema_inside_the_payload() {
        let mut p = producer();
        let frames = p.interval(&rows(3, 2_000), Vec::new(), (0, 0), 7, |_| true);

        let Some(Frame::Rows { rows, .. }) = frames.last() else {
            panic!("a rows frame closes the interval")
        };
        let decoded = crate::recorder::wal::decode_wal_group_row(&rows[0].row).unwrap();
        assert_eq!(
            decoded.schema.as_ref().map(|s| s.counters.len()),
            Some(3),
            "the payload anchors its own schema"
        );
        assert_eq!(decoded.schema_hash, schema(3).hash());
        assert_eq!(
            decoded.counters,
            vec![Some(0), Some(1), Some(2)],
            "and the values came through untouched"
        );
    }

    /// And then stops carrying it. This is the saving the row endpoint already
    /// measured at 2.6x, kept rather than given back.
    #[test]
    fn a_schema_already_sent_is_not_sent_again() {
        let mut p = producer();
        p.interval(&rows(3, 2_000), Vec::new(), (0, 0), 7, |_| true);
        let frames = p.interval(&rows(3, 3_000), Vec::new(), (0, 0), 8, |_| true);

        let Some(Frame::Rows { rows, .. }) = frames.last() else {
            panic!("a rows frame")
        };
        let decoded = crate::recorder::wal::decode_wal_group_row(&rows[0].row).unwrap();
        assert!(decoded.schema.is_none(), "the subscriber already has it");
        assert_eq!(
            decoded.schema_hash,
            schema(3).hash(),
            "the hash stays, so the subscriber knows which schema to resolve"
        );
    }

    /// A changed schema is re-anchored. Tracked per stream by hash, so a group
    /// whose membership moved re-sends and its neighbours do not.
    #[test]
    fn a_changed_schema_is_sent_again() {
        let mut p = producer();
        p.interval(&rows(3, 2_000), Vec::new(), (0, 0), 7, |_| true);
        let frames = p.interval(&rows(4, 3_000), Vec::new(), (0, 0), 8, |_| true);

        let Some(Frame::Rows { rows, .. }) = frames.last() else {
            panic!("a rows frame")
        };
        let decoded = crate::recorder::wal::decode_wal_group_row(&rows[0].row).unwrap();
        assert_eq!(decoded.schema.map(|s| s.counters.len()), Some(4));
    }

    /// Metadata precedes data within an interval, so a row can never reference
    /// identity the subscriber has not received. Order is the covenant that
    /// makes one connection enough.
    #[test]
    fn index_entries_come_before_the_rows_they_describe() {
        let mut p = producer();
        let mut index = SourceIndex::new();
        let entry = index
            .observe(
                STREAM,
                vec![(
                    0u32,
                    [("comm".to_string(), "redis".to_string())]
                        .into_iter()
                        .collect(),
                )],
            )
            .unwrap();

        let frames = p.interval(
            &rows(3, 2_000),
            vec![(STREAM.to_string(), entry)],
            index.state(),
            7,
            |_| true,
        );

        assert!(matches!(frames[0], Frame::Index { .. }), "index first");
        assert!(matches!(frames[1], Frame::Rows { .. }), "then rows");
        assert_eq!(frames.len(), 2);
    }

    /// A frame carries the stamp of the PASS it describes, not the moment it
    /// was emitted.
    ///
    /// The two differ by however long the snapshot sat in the TTL cache. A
    /// subscriber's interval does not drive sampling — that is #1226's whole
    /// point — so a frame sent now routinely describes values read earlier,
    /// and stamping at emission would date every reading to when it was sent.
    /// The producer used to call `Instant::now()` here; this is what says it
    /// must not.
    ///
    /// `wall_offset` rides along untouched for the same reason, so
    /// `ts + wall_offset` still recovers the wall clock AT THE READ rather
    /// than at the send — which is what makes an NTP step locate to the tick
    /// it happened on.
    #[test]
    fn a_frame_carries_the_stamp_of_the_pass_not_of_the_send() {
        let mut p = producer();
        // A pass that ran a second ago, when the wall clock disagreed with the
        // timeline by five seconds — the step this design exists for.
        let pass_ts = 1_700_000_000_000_000_000i64;
        let pass_offset = 5_000_000_000i64;
        let mut tick = rows(3, (pass_ts + pass_offset) as u64);
        tick.ts = pass_ts;
        tick.wall_offset = pass_offset;

        let frames = p.interval(&tick, Vec::new(), (0, 0), 7, |_| true);

        let Some(Frame::Rows { rows, .. }) = frames.last() else {
            panic!("a rows frame")
        };
        let row = &rows[0];
        assert_eq!(
            row.ts, pass_ts,
            "the frame is stamped when the values were read, not when it was sent"
        );
        assert_eq!(
            row.ts + row.wall_offset,
            (pass_ts + pass_offset),
            "ts + wall_offset is the wall clock at the read"
        );
        assert_ne!(
            row.ts,
            crate::agent::epoch::anchored_ts(std::time::Instant::now()),
            "and emphatically not the moment of the send"
        );
    }

    /// The whole tick, through the real codec and into a real archive. Frames
    /// that round-tripped through the types but not the wire would not prove
    /// the subscriber can read what this produces.
    #[test]
    fn a_tick_survives_the_wire_and_lands_in_an_archive() {
        use dendro::replicate::wire::{self, FrameReader};
        use dendro::replicate::Subscriber;
        use dendro::segment::SegmentEncoder;
        use dendro::writer::Writer;

        struct NoSegments;
        impl SegmentEncoder for NoSegments {
            fn encode(
                &self,
                _: &str,
                _: &[dendro::archive::WalRow],
            ) -> dendro::segment::EncodeResult {
                Ok(None)
            }
            fn version(&self) -> Option<&str> {
                Some("rez-frame-producer")
            }
        }

        let mut p = producer();
        let mut index = SourceIndex::new();
        let entry = index
            .observe(
                STREAM,
                vec![(
                    0u32,
                    [("comm".to_string(), "redis".to_string())]
                        .into_iter()
                        .collect(),
                )],
            )
            .unwrap();

        let mut sent = vec![p.handshake()];
        sent.extend(p.interval(
            &rows(3, 2_000),
            vec![(STREAM.to_string(), entry)],
            index.state(),
            7,
            |_| true,
        ));

        let mut stream = Vec::new();
        wire::write_preamble(&mut stream).unwrap();
        for frame in &sent {
            wire::encode_frame(frame, &mut stream).unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subscribed.dendro");
        let mut subscriber = Subscriber::new(Writer::create(&path, Box::new(NoSegments)).unwrap());
        let mut reader = FrameReader::new(std::io::Cursor::new(stream)).unwrap();
        let (mut applied_rows, mut skipped) = (0usize, 0usize);
        while let Some(frame) = reader.next_frame().unwrap() {
            let applied = subscriber.apply(frame).unwrap();
            applied_rows += applied.rows;
            skipped += applied.rows_skipped;
        }
        drop(subscriber);

        assert_eq!(skipped, 0, "every row resolved against the index it named");
        assert_eq!(applied_rows, 1);

        let archive = dendro::archive::Archive::open(&path).unwrap();
        let sources = archive.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].uuid.as_deref(),
            Some("11111111-2222-4333-8444-555555555555"),
            "the producer epoch IS the source uuid"
        );

        let stored = archive
            .read_caller_rows(sources[0].id, STREAM, i64::MIN, i64::MAX)
            .unwrap();
        assert_eq!(stored.len(), 1, "the index entry landed in caller_rows");
        let decoded = crate::recorder::index::IndexEntry::decode(&stored[0].blob).unwrap();
        assert_eq!(decoded.slots[0].labels.get("comm").unwrap(), "redis");
    }

    /// Rows built against an index state the subscriber does not hold are
    /// skipped rather than attributed. The producer's job is to never create
    /// that situation; this is what happens when the wire does.
    #[test]
    fn rows_naming_a_state_the_subscriber_lacks_are_skipped() {
        let mut p = producer();
        let frames = p.interval(&rows(3, 2_000), Vec::new(), (0xdead, 0xbeef), 7, |_| true);
        let Some(Frame::Rows { index_state, .. }) = frames.last() else {
            panic!("a rows frame")
        };
        assert_eq!(*index_state, (0xdead, 0xbeef));
    }

    #[test]
    fn an_empty_interval_is_a_rows_frame_with_no_rows() {
        let p = producer();
        let Frame::Rows { seq, rows, .. } = p.empty_interval((1, 2), 99) else {
            panic!("a rows frame")
        };
        assert_eq!(seq, 99, "the interval index, so a gap is still visible");
        assert!(rows.is_empty());
    }

    /// Two subscriptions to one agent are told the same timeline.
    ///
    /// The anchor used to be minted per producer, so each connection
    /// advertised its own `clock_anchor_wall_ns` while naming the same source
    /// uuid. A subscriber trusts the handshake to place every row it receives,
    /// so that was the same source described as having two timelines, offset
    /// by however far the wall clock moved between the connections — and two
    /// recordings of one agent that could not be merged, with nothing saying
    /// why.
    #[test]
    fn every_subscription_is_told_the_same_source_timeline() {
        let anchor_of = |p: &mut FrameProducer| {
            let Frame::Handshake {
                clock_anchor_wall_ns,
                ..
            } = p.handshake()
            else {
                panic!("a handshake")
            };
            clock_anchor_wall_ns
        };

        let first = anchor_of(&mut producer());
        let second = anchor_of(&mut producer());
        assert_eq!(first, second, "two subscriptions, one source, one timeline");
        assert_eq!(
            first,
            crate::agent::epoch::clock_anchor_wall_ns(),
            "and it is the source's anchor, not one this connection invented"
        );
    }

    /// Producer and consumer agree, end to end, through the real codec.
    ///
    /// Every other test here hands the subscriber frames it built itself, which
    /// checks the rules but not the agreement. This drives the AGENT's
    /// `FrameProducer`, encodes what it emits with dendro's wire, reads it back
    /// with dendro's reader, and applies it — so a producer and a consumer that
    /// disagreed about the index state, the frame order, or the encoding would
    /// fail here rather than in the field.
    #[test]
    fn what_the_agent_produces_is_what_this_consumes() {
        use crate::recorder::index::SourceIndex;
        use crate::recorder::stream::StreamSubscriber;

        // This module's own fixtures: one counter group with a schema and
        // an encoded payload, the same shape every other test here uses.
        let rows = rows(1, 2_000);

        // The agent side: an index entry and the rows built against it.
        let mut producer_index = SourceIndex::new();
        let entry = producer_index
            .observe(
                STREAM,
                vec![(
                    0u32,
                    [("comm".to_string(), "redis".to_string())]
                        .into_iter()
                        .collect::<BTreeMap<String, String>>(),
                )],
            )
            .unwrap();
        let mut producer = FrameProducer::new(
            "epoch-1".to_string(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            BTreeMap::new(),
        );

        let mut sent = vec![producer.handshake()];
        sent.extend(producer.interval(
            &rows,
            vec![(STREAM.to_string(), entry)],
            producer_index.state(),
            7,
            |_| true,
        ));

        // Through the actual bytes, not the values.
        let mut bytes = Vec::new();
        dendro::replicate::wire::write_preamble(&mut bytes).unwrap();
        for frame in &sent {
            dendro::replicate::wire::encode_frame(frame, &mut bytes).unwrap();
        }
        let mut reader =
            dendro::replicate::wire::FrameReader::new(std::io::Cursor::new(bytes)).unwrap();
        let mut received = Vec::new();
        while let Some(frame) = reader.next_frame().unwrap() {
            received.push(frame);
        }

        let mut sub = StreamSubscriber::new();
        let applied = sub.apply(received).unwrap();

        assert_eq!(
            applied.rows_skipped, 0,
            "the state the producer stamped is the state the consumer accumulated"
        );
        assert_eq!(applied.rows.len(), 1);
        assert_eq!(applied.seq, 7);
        assert_eq!(
            sub.index().stream(STREAM).unwrap().labels(0).unwrap()["comm"],
            "redis",
            "and the identity came through the blob intact"
        );

        // The payload reaching the recording is the producer's own bytes, with
        // its schema anchored inside — which is what makes this a passthrough
        // rather than a re-encoding.
        let decoded = crate::recorder::wal::decode_wal_group_row(&applied.rows[0].row).unwrap();
        assert_eq!(
            decoded.counters,
            vec![Some(0)],
            "the fixture value, through untouched"
        );
        assert_eq!(
            decoded.schema.map(|s| s.counters.len()),
            Some(1),
            "the first row of a stream anchors its schema"
        );
    }

    /// What an interval costs, steady-state against the tick that re-sends.
    ///
    /// **A model of `delta`, not `delta`.** The measured numbers this is built
    /// to resemble — 49 rows, 45,350 B against 158,360 B depending only on
    /// schema resend, `cpu_usage/cpu_usage_task` at 638 descriptors churning on
    /// 59 of 60 scrapes — came from that host. The acceptance number for
    /// #1224 Phase 2 has to come from there too, once the endpoint serves
    /// these. What this shows is that the mechanism does what it was built to:
    /// the churning group's per-tick cost stops scaling with its size.
    #[test]
    fn an_interval_costs_a_resend_once_and_a_delta_afterwards() {
        const GROUPS: usize = 49;
        const TASKS: u32 = 319;

        let task_labels = |slot: u32, generation: u32| {
            let comm = if slot == 0 && generation == 1 {
                "new_task".to_string()
            } else {
                format!("task_{slot}")
            };
            (
                slot,
                [
                    ("comm".to_string(), comm),
                    ("cgroup".to_string(), "/system.slice".to_string()),
                ]
                .into_iter()
                .collect::<BTreeMap<String, String>>(),
            )
        };

        // One wide churning group plus 48 ordinary ones, which is the shape of
        // a scrape: churn is concentrated, not spread.
        let tick = |generation: u32| {
            let mut rows = vec![AgentRow {
                stream: STREAM.to_string(),
                window: Some((1_000, 2_000)),
                schema_hash: schema(638).hash(),
                schema: Some(schema(638)),
                arity: (638, 0, 0),
                approx_bytes: 8_192,
                row: payload(638),
            }];
            for g in 1..GROUPS {
                rows.push(AgentRow {
                    stream: format!("sampler_{g}/group"),
                    window: Some((1_000, 2_000)),
                    schema_hash: schema(8).hash(),
                    schema: Some(schema(8)),
                    arity: (8, 0, 0),
                    approx_bytes: 256,
                    row: payload(8),
                });
            }
            let _ = generation;
            AgentRows {
                wall_ns: 2_000,
                duration_ns: 1_000,
                ts: 2_000,
                wall_offset: 0,
                rows,
            }
        };

        let encoded = |frames: &[Frame]| -> usize {
            let mut out = Vec::new();
            for f in frames {
                dendro::replicate::wire::encode_frame(f, &mut out).unwrap();
            }
            out.len()
        };

        let mut p = producer();
        let mut index = SourceIndex::new();

        let opening_entry = index
            .observe(STREAM, (0..TASKS).map(|s| task_labels(s, 0)))
            .unwrap();
        let opening = p.interval(
            &tick(0),
            vec![(STREAM.to_string(), opening_entry)],
            index.state(),
            7,
            |_| true,
        );
        let opening_bytes = encoded(&opening);

        // A tick where one task was recycled and nothing else moved: the
        // measured common case.
        let churn_entry = index
            .observe(STREAM, (0..TASKS).map(|s| task_labels(s, 1)))
            .unwrap();
        let steady = p.interval(
            &tick(1),
            vec![(STREAM.to_string(), churn_entry)],
            index.state(),
            8,
            |_| true,
        );
        let steady_bytes = encoded(&steady);

        println!(
            "\n  {GROUPS} rows, {TASKS} tasks, one recycled\n\
               \x20   opening interval (schemas + index Full): {opening_bytes:>7} B\n\
               \x20   steady interval  (one index Delta):      {steady_bytes:>7} B\n"
        );

        assert!(
            steady_bytes * 4 < opening_bytes,
            "a steady interval ({steady_bytes} B) should be a small fraction of the \
             opening one ({opening_bytes} B); identity and schemas are both stated \
             once and then referenced"
        );
    }

    #[test]
    fn the_handshake_pins_the_anchor_it_stamps_rows_against() {
        let mut p = producer();
        assert!(!p.has_sent_handshake());
        let Frame::Handshake {
            clock_anchor_wall_ns,
            complete,
            uuid,
            ..
        } = p.handshake()
        else {
            panic!("a handshake")
        };
        assert!(p.has_sent_handshake());
        assert_eq!(
            clock_anchor_wall_ns,
            crate::agent::epoch::clock_anchor_wall_ns(),
            "the handshake pins the SOURCE's anchor — a subscriber places every \
             row it receives against this"
        );
        assert!(!complete, "a running agent's source has not ended");
        assert_eq!(
            uuid.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }
}

/// Pricing the archive as a replication source — #1224, deciding whether the
/// agent should stream from an in-memory dendro archive or produce frames
/// directly.
///
/// Two paths, same input, same frames out:
///
/// - **direct**: snapshot -> `FrameProducer` -> frames. What the agent does
///   today.
/// - **archive**: snapshot -> dendro `Writer` (tmpfs) -> `ArchivePublisher` ->
///   frames. What it would do if replication came out of an archive, which is
///   what buys backfill by segment frame, hindsight as an ordinary subscriber,
///   and one replication path instead of two.
///
/// The archive path pays a write and a read per tick that the direct path does
/// not. This says what that costs.
#[cfg(test)]
mod archive_as_a_source {
    use super::tests::*;
    use super::*;
    use crate::recorder::wire::{AgentRow, AgentRows};
    use dendro::archive::{Archive, CallerRow, SourceMeta};
    use dendro::replicate::ArchivePublisher;
    use dendro::segment::SegmentEncoder;
    use dendro::writer::Writer;
    use std::time::Duration;

    struct NoSegments;
    impl SegmentEncoder for NoSegments {
        fn encode(&self, _: &str, _: &[dendro::archive::WalRow]) -> dendro::segment::EncodeResult {
            Ok(None)
        }
        fn version(&self) -> Option<&str> {
            Some("rez-archive-pricing")
        }
    }

    fn percentile(sorted: &[Duration], p: f64) -> Duration {
        sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
    }

    /// What the archive hop costs per tick, against producing frames directly.
    ///
    /// `cargo test --release --bin rezolus archive_as_a_source -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn price_the_archive_as_a_replication_source() {
        const TICKS: usize = 600;
        const GROUPS: usize = 49;
        const WIDE: usize = 638;

        let tick = || {
            let mut rows = vec![AgentRow {
                stream: STREAM.to_string(),
                window: Some((1_000, 2_000)),
                schema_hash: schema(WIDE).hash(),
                schema: Some(schema(WIDE)),
                arity: (WIDE as u32, 0, 0),
                approx_bytes: 8_192,
                row: payload(WIDE),
            }];
            for g in 1..GROUPS {
                rows.push(AgentRow {
                    stream: format!("sampler_{g}/group"),
                    window: Some((1_000, 2_000)),
                    schema_hash: schema(8).hash(),
                    schema: Some(schema(8)),
                    arity: (8, 0, 0),
                    approx_bytes: 256,
                    row: payload(8),
                });
            }
            AgentRows {
                wall_ns: 2_000,
                duration_ns: 1_000,
                ts: 2_000,
                wall_offset: 0,
                rows,
            }
        };

        let encoded = |frames: &[Frame]| -> usize {
            let mut out = Vec::new();
            for f in frames {
                dendro::replicate::wire::encode_frame(f, &mut out).unwrap();
            }
            out.len()
        };

        // --- direct ---------------------------------------------------------
        let mut p = producer();
        let mut direct = Vec::with_capacity(TICKS);
        let mut direct_bytes = 0usize;
        for seq in 0..TICKS {
            let t = tick();
            let start = std::time::Instant::now();
            let frames = p.interval(&t, Vec::new(), (0, 0), seq as u64, |_| true);
            direct.push(start.elapsed());
            direct_bytes += encoded(&frames);
        }

        // --- archive --------------------------------------------------------
        // /tmp is tmpfs on the hosts this is measured on, so the archive is
        // genuinely in memory rather than merely uncommitted.
        let dir = tempfile::Builder::new()
            .prefix("rez-price-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("live.dendro");
        let mut archive = Writer::create(&path, Box::new(NoSegments)).unwrap();
        let mut source = archive
            .add_source(SourceMeta {
                labels: [("source".to_string(), "rezolus".to_string())]
                    .into_iter()
                    .collect(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: 1_700_000_000_000_000_000,
            })
            .unwrap();
        let db = Archive::open(&path).unwrap();
        let (mut publisher, _opening) = ArchivePublisher::tailing(&db).unwrap();

        let mut write = Vec::with_capacity(TICKS);
        let mut visible = Vec::with_capacity(TICKS);
        let mut poll = Vec::with_capacity(TICKS);
        let mut archive_bytes = 0usize;
        for seq in 0..TICKS {
            let t = tick();
            let ts = 10_000 + seq as i64 * 1_000_000_000;

            // 1. The write itself. Asynchronous — this hands rows to the
            //    writer thread and returns.
            let start = std::time::Instant::now();
            let wal: Vec<dendro::archive::WalRow> = t
                .rows
                .iter()
                .map(|r| dendro::archive::WalRow {
                    stream: r.stream.clone(),
                    ts,
                    wall_offset: 0,
                    row: r.row.clone(),
                })
                .collect();
            source.wal(wal).unwrap();
            let _ = source.caller_rows(
                STREAM,
                vec![CallerRow {
                    ts,
                    blob: vec![0u8; 80],
                }],
            );
            write.push(start.elapsed());

            // 2. How long until the writer thread has committed it and a
            //    reader can see it, and 3. what the publishing call itself
            //    costs once it can. Kept apart because they answer different
            //    questions: the first is latency a 1s stream would not notice,
            //    the second is CPU on the agent.
            //
            //    Polled with a sleep rather than a spin, and against ONE open
            //    archive rather than reopening per attempt — a spin charges
            //    the wait to CPU, and reopening charges every attempt a fresh
            //    SQLite open, neither of which a real agent would do.
            let waited = std::time::Instant::now();
            loop {
                let call = std::time::Instant::now();
                let frames = publisher.next(&db).expect("tailing keeps up");
                let elapsed = call.elapsed();
                let rows: usize = frames
                    .iter()
                    .map(|f| match f {
                        Frame::Rows { rows, .. } => rows.len(),
                        _ => 0,
                    })
                    .sum();
                if rows > 0 {
                    visible.push(waited.elapsed());
                    poll.push(elapsed);
                    archive_bytes += encoded(&frames);
                    break;
                }
                if waited.elapsed() > Duration::from_secs(5) {
                    panic!("the tick never became visible to the publisher");
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        }

        // Does the publish cost grow as the live WAL tail grows? This
        // benchmark never seals, so if it does, the headline p50 is an
        // artifact of an unbounded tail rather than a steady-state cost.
        let quarter = TICKS / 4;
        let mut first: Vec<Duration> = poll[..quarter].to_vec();
        let mut last: Vec<Duration> = poll[TICKS - quarter..].to_vec();
        first.sort();
        last.sort();
        println!(
            "  publish cost, first {quarter} ticks p50 {:.1?} -> last {quarter} p50 {:.1?}",
            percentile(&first, 0.50),
            percentile(&last, 0.50),
        );

        for v in [&mut direct, &mut write, &mut visible, &mut poll] {
            v.sort();
        }
        let line = |name: &str, v: &[Duration]| {
            println!(
                "  {name:<22} p50 {:>9.1?}  p99 {:>9.1?}  max {:>9.1?}",
                percentile(v, 0.50),
                percentile(v, 0.99),
                percentile(v, 1.0),
            );
        };

        println!("\n{TICKS} ticks, {GROUPS} groups ({WIDE} slots on the wide one)\n");
        line("direct: produce", &direct);
        line("archive: write (async)", &write);
        line("archive: publish (cpu)", &poll);
        line("archive: time to visible", &visible);
        let direct_cpu = percentile(&direct, 0.50);
        let archive_cpu = percentile(&write, 0.50) + percentile(&poll, 0.50);
        println!(
            "\n  CPU per tick at p50: direct {:.1?}, archive {:.1?} ({:.1}x)",
            direct_cpu,
            archive_cpu,
            archive_cpu.as_secs_f64() / direct_cpu.as_secs_f64(),
        );
        println!(
            "  the archive also adds {:.1?} p50 / {:.1?} p99 of commit latency before a \n  \
             tick can be published at all",
            percentile(&visible, 0.50),
            percentile(&visible, 0.99),
        );
        println!("  bytes out: direct {direct_bytes}, archive {archive_bytes}\n");
    }
}
