//! The properties rezolus needs dendro's replication to keep.
//!
//! `crates/rez` already turns an agent snapshot into WAL rows keyed by
//! `(sampler, ts)` with an opaque msgpack payload, which is the shape
//! `dendro::replicate::Frame::Rows` carries. These drive a rezolus-shaped tick
//! through dendro's frames, its wire codec and its `Subscriber`.
//!
//! Written as a fit probe before adopting dendro's replication, and kept
//! because dendro asked for it to be kept: a probe says the seam fit on the
//! day it was run, a test says it still does. The dependency is a git rev
//! until dendro releases, so these are what notice when a revision moves
//! something rezolus leans on.
//!
//! The producer they describe does not exist yet — that is #1224's Phase 2.
//! What is pinned here is the contract it will be written against.

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use dendro::archive::{Archive, WalRow};
    use dendro::replicate::wire::{self, FrameReader};
    use dendro::replicate::{Frame, IndexKind, Subscriber, NO_INDEX_STATE};
    use dendro::segment::SegmentEncoder;
    use dendro::writer::Writer;
    use std::collections::BTreeMap;

    const ANCHOR: i64 = 1_700_000_000_000_000_000;

    /// A stand-in for rezolus's parquet encoder. The fit question is about
    /// frames and types, not about how a segment is built, and dendro takes
    /// the encoder from the caller either way.
    struct NoSegments;

    impl SegmentEncoder for NoSegments {
        fn encode(&self, _stream: &str, _rows: &[WalRow]) -> dendro::segment::EncodeResult {
            Ok(None)
        }
        fn version(&self) -> Option<&str> {
            Some("rez-fit-probe")
        }
    }

    /// One tick, the way rezolus would emit it: identity first, then rows that
    /// name the state they were built against.
    #[test]
    fn a_rezolus_shaped_tick_survives_the_frames_the_codec_and_a_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subscribed.dendro");

        // rezolus's own identity: the producer epoch IS the source uuid, since
        // both mean "this producer until it restarts".
        let uuid = "11111111-2222-4333-8444-555555555555".to_string();
        let handshake = Frame::Handshake {
            source: 0,
            uuid: Some(uuid.clone()),
            labels: BTreeMap::from([("source".to_string(), "rezolus".to_string())]),
            metadata: BTreeMap::from([(dendro::keys::PRODUCER_EPOCH.to_string(), uuid.clone())]),
            clock_anchor_wall_ns: ANCHOR,
            complete: false,
        };

        // An index entry. The blob is rezolus's to define and dendro never
        // decodes it, so any bytes stand in here.
        let index_blob = rmp_serde::to_vec(&vec![(17u32, "cpu=11")]).unwrap();
        let state = (0xfeed_u64, 0xface_u64);
        let index = Frame::Index {
            source: 0,
            stream: "cpu_usage/usage".to_string(),
            ts: ANCHOR,
            kind: IndexKind::Full,
            state,
            blob: index_blob.clone(),
        };

        // Rows in exactly the shape `stage_rows` already produces: a group
        // name as the stream, an opaque msgpack payload.
        let rows = Frame::Rows {
            source: 0,
            seq: 0,
            index_state: state,
            rows: vec![WalRow {
                stream: "cpu_usage/usage".to_string(),
                ts: ANCHOR,
                wall_offset: 0,
                row: vec![0x93, 0x01, 0x02, 0x03],
            }],
        };

        // Through the actual wire — preamble and framing — not merely through
        // the types. A frame that failed to round-trip would still apply if we
        // handed the subscriber the value we had just built.
        let sent = [handshake, index, rows];
        let mut stream = Vec::new();
        wire::write_preamble(&mut stream).expect("preamble");
        for frame in &sent {
            wire::encode_frame(frame, &mut stream).expect("encode");
        }

        let mut subscriber =
            Subscriber::new(Writer::create(&path, Box::new(NoSegments)).expect("create"));
        let mut reader = FrameReader::new(std::io::Cursor::new(stream)).expect("preamble reads");
        let mut received = 0usize;
        while let Some(frame) = reader.next_frame().expect("decode") {
            assert_eq!(frame, sent[received], "a frame changed shape on the wire");
            subscriber.apply(frame).expect("apply");
            received += 1;
        }
        assert_eq!(received, sent.len(), "every frame came back");
        drop(subscriber);

        // And it reads back as an archive.
        let archive = Archive::open(&path).expect("open");
        let sources = archive.read_sources().expect("sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].uuid.as_deref(), Some(uuid.as_str()));

        let stored = archive
            .read_caller_rows(sources[0].id, "cpu_usage/usage", i64::MIN, i64::MAX)
            .expect("caller rows");
        assert_eq!(stored.len(), 1, "the index entry landed in caller_rows");
        assert_eq!(stored[0].blob, index_blob, "and it was stored verbatim");
    }

    /// An index whose first `Full` is **empty** must still count as having
    /// arrived.
    ///
    /// This is the shape rezolus will be in on first connect: the agent
    /// attaches, and its first index batch has nothing in it yet because no
    /// cgroup or task has been assigned a slot. dendro found and fixed a
    /// defect here while reviewing its own #4 — an empty opening batch was
    /// treated as not having sent its `Full`, so every later entry was a
    /// `Delta` and the subscriber skipped every row for the life of the
    /// connection, visible only as a rising `rows_skipped`.
    ///
    /// Kept because "the failure is invisible unless you look at a counter" is
    /// exactly the kind of regression a version bump reintroduces quietly.
    #[test]
    fn an_empty_opening_index_still_admits_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty-first-index.dendro");
        let mut subscriber =
            Subscriber::new(Writer::create(&path, Box::new(NoSegments)).expect("create"));

        subscriber
            .apply(Frame::Handshake {
                source: 0,
                uuid: None,
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: ANCHOR,
                complete: false,
            })
            .expect("handshake");

        // An agent with nothing assigned yet: a Full carrying no slots.
        let empty_state = (1, 0);
        subscriber
            .apply(Frame::Index {
                source: 0,
                stream: "cpu_usage/usage".to_string(),
                ts: ANCHOR,
                kind: IndexKind::Full,
                state: empty_state,
                blob: rmp_serde::to_vec(&Vec::<(u32, String)>::new()).unwrap(),
            })
            .expect("empty full");

        // Rows built against that same empty state must be accepted. Before
        // dendro's fix they were skipped, and would have gone on being skipped
        // for every later state too.
        let applied = subscriber
            .apply(Frame::Rows {
                source: 0,
                seq: 0,
                index_state: empty_state,
                rows: vec![WalRow {
                    stream: "cpu_usage/usage".to_string(),
                    ts: ANCHOR,
                    wall_offset: 0,
                    row: vec![0x91, 0x01],
                }],
            })
            .expect("rows");

        assert_eq!(
            applied.rows_skipped, 0,
            "an empty opening Full is still a Full; rows against it must not be skipped"
        );
        assert_eq!(applied.rows, 1, "the row should have been written");
    }

    /// The safety rule rezolus depends on: rows built against index state the
    /// subscriber does not hold must be refused rather than attributed to
    /// whatever it does hold.
    #[test]
    fn rows_naming_an_index_state_the_subscriber_lacks_are_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mismatch.dendro");
        let mut subscriber =
            Subscriber::new(Writer::create(&path, Box::new(NoSegments)).expect("create"));

        subscriber
            .apply(Frame::Handshake {
                source: 0,
                uuid: None,
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: ANCHOR,
                complete: false,
            })
            .expect("handshake");

        // No Index frame was sent, so the subscriber's state is NO_INDEX_STATE.
        let applied = subscriber.apply(Frame::Rows {
            source: 0,
            seq: 0,
            index_state: (0xdead, 0xbeef),
            rows: vec![WalRow {
                stream: "cpu_usage/usage".to_string(),
                ts: ANCHOR,
                wall_offset: 0,
                row: vec![0xc0],
            }],
        });

        // Not an error, and not silently applied: `Applied` reports the
        // disposition, which is what lets a consumer notice a gap rather than
        // discover it later in the data.
        let applied = applied.expect("a mismatch is a skip, not a transport error");
        assert_eq!(
            applied.rows, 0,
            "nothing may be written against unknown state"
        );
        assert_eq!(
            applied.rows_skipped, 1,
            "and the skip has to be reported, not swallowed"
        );
        assert_ne!(NO_INDEX_STATE, (0xdead, 0xbeef));
    }
}
