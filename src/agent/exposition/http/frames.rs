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
//! So `ts` is anchored here: `anchor_wall_ns + monotonic elapsed`, which has
//! wall-clock magnitude but advances monotonically, so rows stay strictly
//! increasing through an NTP step. `wall_offset` is wall minus `ts` at the
//! moment of the read, so `ts + wall_offset` recovers the real wall clock and
//! the divergence is visible rather than absorbed.
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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
    /// The wall clock read once, with the monotonic instant it was read at.
    /// Together they are the anchored timeline — see the module docs.
    anchor_wall_ns: i64,
    anchor: Instant,
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
        Self::anchored_at(epoch, labels, metadata, SystemTime::now(), Instant::now())
    }

    /// The anchor injected, so a test can state the timeline rather than
    /// observe it.
    fn anchored_at(
        epoch: String,
        labels: BTreeMap<String, String>,
        metadata: BTreeMap<String, String>,
        wall: SystemTime,
        anchor: Instant,
    ) -> Self {
        Self {
            uuid: epoch,
            labels,
            metadata,
            anchor_wall_ns: wall
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            anchor,
            sent_schemas: BTreeMap::new(),
            handshake_sent: false,
        }
    }

    /// `anchor_wall_ns + monotonic elapsed` — FORMAT.md §5's anchored `ts`.
    fn ts_at(&self, now: Instant) -> i64 {
        self.anchor_wall_ns + now.saturating_duration_since(self.anchor).as_nanos() as i64
    }

    /// The opening frame. Sent once per connection, before anything else.
    pub(crate) fn handshake(&mut self) -> Frame {
        self.handshake_sent = true;
        Frame::Handshake {
            source: SOURCE,
            uuid: Some(self.uuid.clone()),
            labels: self.labels.clone(),
            metadata: self.metadata.clone(),
            clock_anchor_wall_ns: self.anchor_wall_ns,
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
        now: Instant,
        rows: &AgentRows,
        entries: Vec<(String, IndexEntry)>,
        index_state: IndexState,
        seq: u64,
    ) -> Vec<Frame> {
        let ts = self.ts_at(now);
        let wall_offset = rows.wall_ns as i64 - ts;

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

    const STREAM: &str = "cpu_usage/cpu_usage_task";

    fn schema(n: usize) -> crate::recorder::schema::GroupSchema {
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

    fn payload(n: usize) -> Vec<u8> {
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

    fn rows(n: usize, wall_ns: u64) -> AgentRows {
        AgentRows {
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

    fn producer() -> FrameProducer {
        FrameProducer::anchored_at(
            "11111111-2222-4333-8444-555555555555".to_string(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            BTreeMap::new(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            Instant::now(),
        )
    }

    /// The schema is in the ENVELOPE on the row endpoint and has to be in the
    /// PAYLOAD here, because a `WalRow` has no envelope. A producer that moved
    /// the row across without moving the schema would hand the subscriber a
    /// payload naming a `schema_hash` it has no way to resolve.
    #[test]
    fn the_first_row_of_a_stream_carries_its_schema_inside_the_payload() {
        let mut p = producer();
        let frames = p.interval(Instant::now(), &rows(3, 2_000), Vec::new(), (0, 0), 7);

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
        p.interval(Instant::now(), &rows(3, 2_000), Vec::new(), (0, 0), 7);
        let frames = p.interval(Instant::now(), &rows(3, 3_000), Vec::new(), (0, 0), 8);

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
        p.interval(Instant::now(), &rows(3, 2_000), Vec::new(), (0, 0), 7);
        let frames = p.interval(Instant::now(), &rows(4, 3_000), Vec::new(), (0, 0), 8);

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
            Instant::now(),
            &rows(3, 2_000),
            vec![(STREAM.to_string(), entry)],
            index.state(),
            7,
        );

        assert!(matches!(frames[0], Frame::Index { .. }), "index first");
        assert!(matches!(frames[1], Frame::Rows { .. }), "then rows");
        assert_eq!(frames.len(), 2);
    }

    /// `ts` is anchored and `wall_offset` recovers the wall clock — FORMAT.md
    /// §5. The anchor is injected, so this states the timeline rather than
    /// observing whatever the machine's clock did.
    #[test]
    fn ts_is_anchored_and_wall_offset_recovers_the_wall_clock() {
        let anchor = Instant::now();
        let mut p = FrameProducer::anchored_at(
            "epoch".to_string(),
            BTreeMap::new(),
            BTreeMap::new(),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            anchor,
        );
        let anchor_ns = 1_700_000_000_000_000_000i64;

        // A tick one second of MONOTONIC time later, whose wall clock says
        // something else entirely — the NTP step this design exists for.
        let stepped_wall = (anchor_ns + 5_000_000_000) as u64;
        let frames = p.interval(
            anchor + Duration::from_secs(1),
            &rows(3, stepped_wall),
            Vec::new(),
            (0, 0),
            7,
        );

        let Some(Frame::Rows { rows, .. }) = frames.last() else {
            panic!("a rows frame")
        };
        let row = &rows[0];
        assert_eq!(
            row.ts,
            anchor_ns + 1_000_000_000,
            "ts advanced by monotonic elapsed, not by the wall clock"
        );
        assert_eq!(
            row.ts + row.wall_offset,
            stepped_wall as i64,
            "ts + wall_offset is the wall clock at the read"
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
            Instant::now(),
            &rows(3, 2_000),
            vec![(STREAM.to_string(), entry)],
            index.state(),
            7,
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
        let frames = p.interval(
            Instant::now(),
            &rows(3, 2_000),
            Vec::new(),
            (0xdead, 0xbeef),
            7,
        );
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

    /// The real constructor anchors to the clock rather than to anything a
    /// test hands it. Every other test here injects the anchor, so without
    /// this one `new` would go the whole way to production unexercised.
    #[test]
    fn new_anchors_to_the_wall_clock_it_was_built_at() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let mut p = FrameProducer::new("epoch".to_string(), BTreeMap::new(), BTreeMap::new());
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;

        let Frame::Handshake {
            clock_anchor_wall_ns,
            ..
        } = p.handshake()
        else {
            panic!("a handshake")
        };
        assert!(
            (before..=after).contains(&clock_anchor_wall_ns),
            "the anchor is the wall clock at construction: {clock_anchor_wall_ns} \
             outside {before}..={after}"
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
            Instant::now(),
            &tick(0),
            vec![(STREAM.to_string(), opening_entry)],
            index.state(),
            7,
        );
        let opening_bytes = encoded(&opening);

        // A tick where one task was recycled and nothing else moved: the
        // measured common case.
        let churn_entry = index
            .observe(STREAM, (0..TASKS).map(|s| task_labels(s, 1)))
            .unwrap();
        let steady = p.interval(
            Instant::now(),
            &tick(1),
            vec![(STREAM.to_string(), churn_entry)],
            index.state(),
            8,
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
        assert_eq!(clock_anchor_wall_ns, 1_700_000_000_000_000_000);
        assert!(!complete, "a running agent's source has not ended");
        assert_eq!(
            uuid.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }
}
