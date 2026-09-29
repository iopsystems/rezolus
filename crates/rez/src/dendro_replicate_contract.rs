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
    use dendro::replicate::{Frame, Subscriber, NO_INDEX_STATE};
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

    /// One tick, the way rezolus emits it: a handshake, then rows naming
    /// `NO_INDEX_STATE`, since the agent keeps no identity index.
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

        // Rows in exactly the shape `stage_rows` already produces: a group
        // name as the stream, an opaque msgpack payload.
        let rows = Frame::Rows {
            source: 0,
            seq: 0,
            index_state: NO_INDEX_STATE,
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
        let sent = [handshake, rows];
        let mut stream = Vec::new();
        wire::write_preamble(&mut stream).expect("preamble");
        for frame in &sent {
            wire::encode_frame(frame, &mut stream).expect("encode");
        }

        let mut subscriber =
            Subscriber::new(Writer::create(&path, Box::new(NoSegments)).expect("create"));
        let mut reader = FrameReader::new(std::io::Cursor::new(stream)).expect("preamble reads");
        let mut received = 0usize;
        let mut applied_rows = 0usize;
        while let Some(frame) = reader.next_frame().expect("decode") {
            assert_eq!(frame, sent[received], "a frame changed shape on the wire");
            let applied = subscriber.apply(frame).expect("apply");
            assert_eq!(applied.rows_skipped, 0, "NO_INDEX_STATE always resolves");
            applied_rows += applied.rows;
            received += 1;
        }
        assert_eq!(applied_rows, 1, "the row was written");
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
        assert!(stored.is_empty(), "no index frame, so no caller rows");
    }
}
