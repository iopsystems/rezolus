//! The row-format wire: what an agent serves to a consumer that wants WAL
//! rows rather than a snapshot.
//!
//! # Why this exists
//!
//! Mostly not to save an encode. That was the original argument, and
//! measurement puts it second: moving the same payload over this wire instead
//! of a snapshot is worth 1.71x on the recorder and 1.33x on total CPU —
//! real, but a fraction of what the schema policy below is worth, and part of
//! even that is the row decode not being hardened the way
//! `Snapshot::from_msgpack` is. Under a no-resend policy the transport alone
//! goes NEGATIVE on total CPU (0.90x), because once schemas are gone the
//! per-row framing is what is left.
//!
//! What it is actually for is the schema. An agent serving `/metrics/binary`
//! ships every acquisition group's full `GroupSchema` — every member's name
//! and metadata `BTreeMap` — on **every scrape**, because it has to: the
//! exporter, the parquet recorder and the live viewer all decode one snapshot
//! in isolation and hold no cache to resolve a schema reference against. A
//! schema that repeats unchanged for hours is re-encoded, re-transmitted and
//! re-decoded every tick, and it dwarfs the values it describes: on a
//! 25-sampler host it is 87.8% of the body. Serving rows instead measured
//! **2.6x smaller bodies** end to end on that host — see
//! `docs/journal/2026-09-16-row-endpoint-schema-resend.md`, which also has
//! the two reasons it is 2.6x rather than the 8x that removing schemas
//! outright would give.
//!
//! [`WalGroupRow`](crate::wal::WalGroupRow) was built for a producer that
//! does not do that: `schema` is optional, `schema_hash` identifies the
//! generation, and [`StreamRecorderV3`]'s ring resolves a reference. That
//! machinery has never had a producer — the recorder's own comments note
//! that the `schema: None` path is reached only by malformed input.
//!
//! This wire is that producer. It is worth having precisely because its
//! consumer is a recorder, the one consumer that keeps state across scrapes
//! and can therefore be told "the same schema as last time". Every other
//! endpoint must keep resending, which is why this is a new endpoint rather
//! than a change to an existing one.
//!
//! # The part that is not obvious
//!
//! A [`WalGroupRow`] is *almost* a pure function of the producer's
//! [`GroupSnapshot`], but not quite: its `schema` field is `Some` only on the
//! row that re-anchors a group's schema for **the segment currently
//! accumulating in the consumer's archive**. Segments are the consumer's
//! business — a recorder's rotations have nothing to do with when an agent
//! was scraped, and two consumers of the same agent rotate independently. The
//! agent cannot know when a row needs to carry a schema, so it never decides:
//! every [`AgentRow`] carries a payload with `schema: None`, and the anchoring
//! stays exactly where it was, in [`StreamRecorderV3`].
//!
//! [`StreamRecorderV3`]: crate::rez_v3_writer::StreamRecorderV3
//!
//! That is why the consumer still has to be able to *produce* an anchored row
//! — and it can, by decoding the payload and re-encoding it with a schema
//! attached. The cost is bounded by how often that happens: once per group
//! per segment, against a default seal policy of 300 s. At a 1 s interval,
//! 299 rows out of 300 are copied to the WAL untouched.
//!
//! # What travels in the clear, and why each field does
//!
//! Everything the consumer needs to reach the "does this row need an anchor"
//! decision has to be readable WITHOUT decoding the payload, or the pipe
//! decodes every row and the whole exercise is pointless. That is the entire
//! selection rule for [`AgentRow`]'s fields:
//!
//! | field | the decision it serves |
//! |---|---|
//! | `stream` | which table, and the dedup/anchor state to consult |
//! | `window` | window-advance dedup — a repeat costs nothing at all |
//! | `schema_hash` | schema-ring lookup, and the anchor comparison |
//! | `schema` | teaches the consumer's ring on the producer's cache miss |
//! | `arity` | the validation `GroupSnapshot::validate` does, undecoded |
//! | `approx_bytes` | the seal policy's size accounting, undecoded |
//!
//! `arity` is the one that looks redundant and is not. The snapshot path
//! rejects a group whose value vectors disagree with its schema, and a row
//! path that skipped that check would write WAL rows the snapshot path would
//! have refused — so a recording's contents would depend on which endpoint
//! made it. Three integers keep the two paths agreeing on what is
//! well-formed, with no decode.
//!
//! `approx_bytes` is there for a narrower reason: it is what the seal policy
//! meters segments by, and computing it from the payload would mean decoding
//! the payload. Letting the producer compute it — with the same function the
//! snapshot path uses — is what makes a recording taken over this wire
//! segment at the same boundaries as one taken over the snapshot endpoint,
//! rather than merely containing the same values.

use serde::{Deserialize, Serialize};

use crate::schema::GroupSchema;

/// One acquisition group's tick, encoded by the producer.
///
/// The payload is a [`WalGroupRow`](crate::wal::WalGroupRow) with `schema:
/// None` — see the module docs for why the producer never anchors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    /// The group's name, `"<sampler>/<group>"` — already the archive's table
    /// key, used verbatim.
    pub stream: String,
    /// The acquisition window as `(begin_ns, end_ns)`, in the clear so a
    /// consumer can apply window-advance dedup without touching `row`.
    pub window: Option<(u64, u64)>,
    /// The content hash of the schema `row`'s values align with.
    pub schema_hash: (u64, u64),
    /// The schema itself, present when the PRODUCER's own schema cache
    /// missed — the same rule its snapshot payload follows. A consumer that
    /// already knows this hash ignores it; one that does not, and receives
    /// `None`, cannot decode the row and skips it.
    pub schema: Option<GroupSchema>,
    /// `(counters, gauges, histograms)` slot counts for `row`, so a consumer
    /// can run the arity check against a resolved schema without decoding.
    pub arity: (u32, u32, u32),
    /// `rez::group_approx_bytes` for this group — the seal policy's size
    /// meter, computed by the producer because the consumer would have to
    /// decode the payload to arrive at it.
    pub approx_bytes: u32,
    /// The encoded `WalGroupRow`.
    pub row: Vec<u8>,
}

impl AgentRow {
    /// Rebuild the envelope from a payload that arrived without one.
    ///
    /// The replication stream carries a group's tick as a bare
    /// [`WalGroupRow`](crate::wal::WalGroupRow): `stream`, a stamp, and the
    /// encoded row. Every cleartext field above is inside that payload, so the
    /// subscriber decodes it once here and hands the result to the same
    /// [`StreamRecorderV3::stage_rows`] the row endpoint feeds — decision for
    /// decision, rather than a third copy of the staging rules keyed on a
    /// third shape. The decode is the cost of that reuse; `stage_rows` then
    /// touches the payload only on the tick it re-anchors.
    ///
    /// A payload that carries a schema — the producer's first mention of that
    /// generation on a connection — has it lifted into the envelope and is
    /// re-encoded without it, so `row` keeps the contract the module docs
    /// state: the consumer decides where a schema anchors, never the producer.
    /// Every other payload passes through as the bytes that arrived.
    ///
    /// `approx_bytes` is recomputed with the formula the snapshot path uses
    /// (`wal_group_row_approx_bytes`), so a recording taken off the stream
    /// seals at the same rows as one scraped.
    ///
    /// [`StreamRecorderV3::stage_rows`]: crate::rez_v3_writer::StreamRecorderV3::stage_rows
    pub fn from_payload(stream: String, payload: Vec<u8>) -> Result<Self, String> {
        let mut decoded = crate::wal::decode_wal_group_row(&payload)
            .map_err(|e| format!("stream {stream}: {e}"))?;
        let arity = (
            decoded.counters.len() as u32,
            decoded.gauges.len() as u32,
            decoded.histograms.len() as u32,
        );
        let approx_bytes = crate::rez::wal_group_row_approx_bytes(&decoded) as u32;
        let (schema, row) = match decoded.schema.take() {
            Some(schema) => {
                let row = crate::wal::encode_wal_group_row(&decoded)
                    .map_err(|e| format!("stream {stream}: {e}"))?;
                (Some(schema), row)
            }
            None => (None, payload),
        };
        Ok(AgentRow {
            stream,
            window: decoded.window,
            schema_hash: decoded.schema_hash,
            schema,
            arity,
            approx_bytes,
            row,
        })
    }
}

/// One scrape's worth of rows: the row-format equivalent of a `SnapshotV3`.
///
/// `wall_ns` and `duration_ns` mirror `SnapshotV3`'s `systemtime`/`duration`,
/// which a consumer needs for the same reasons it does there — clock
/// reconciliation and the sampling-latency record.
///
/// # The producer stamps the tick
///
/// `ts` and `wall_offset` are the producer's, and they describe when it READ
/// the values, not when it answered. Those differ: the agent caches a pass for
/// its TTL, so a request arriving inside that window is answered from the
/// cache, and a consumer cannot tell that from one HTTP response. Stamping on
/// the consumer would record the moment it asked and attribute values to it —
/// wrong by up to a TTL on a scrape, and by the transport delay on a stream,
/// where there is no consumer tick at all.
///
/// `ts` is anchored (`clock_anchor_wall_ns + monotonic elapsed`) so it cannot
/// go backwards through a clock step; `ts + wall_offset` recovers the wall
/// clock, which is the same value `wall_ns` carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRows {
    /// The producer's wall clock when the scrape was taken, in nanoseconds
    /// since the Unix epoch.
    pub wall_ns: u64,
    /// How long the producer's sampling pass took, in nanoseconds.
    pub duration_ns: u64,
    /// The pass's stamp on the producer's timeline.
    pub ts: i64,
    /// Wall clock minus `ts` at the pass, so `ts + wall_offset == wall_ns`.
    pub wall_offset: i64,
    pub rows: Vec<AgentRow>,
}

/// The `Content-Type` the row endpoint serves and a consumer asks for.
///
/// Distinct from the snapshot endpoint's, because the two bodies are NOT
/// interchangeable and a consumer that received the wrong one must fail
/// loudly rather than decode garbage into a recording.
pub const CONTENT_TYPE: &str = "application/vnd.rezolus.rows.v1+msgpack";

pub fn encode(rows: &AgentRows) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec(rows).map_err(|e| format!("failed to encode agent rows: {e}"))
}

/// The inverse of [`encode`].
pub fn decode(bytes: &[u8]) -> Result<AgentRows, String> {
    rmp_serde::from_slice(bytes).map_err(|e| format!("failed to decode agent rows: {e}"))
}

/// Encode one producer-side acquisition group as an [`AgentRow`].
///
/// `schema` is what the producer chose to transmit this tick — its own cache
/// decision, passed through unchanged. It is NOT what the payload carries:
/// the payload's schema is always `None`. See the module docs.
#[cfg(feature = "write")]
pub fn encode_group(g: &metriken_exposition::GroupSnapshot) -> Result<AgentRow, String> {
    Ok(AgentRow {
        stream: g.name.clone(),
        window: g.window.map(|w| (w.begin_ns, w.end_ns)),
        schema_hash: g.schema_hash,
        schema: g.schema.as_ref().map(|s| s.as_ref().into()),
        arity: (
            g.counters.len() as u32,
            g.gauges.len() as u32,
            g.histograms.len() as u32,
        ),
        approx_bytes: crate::rez::group_approx_bytes(g) as u32,
        row: crate::wal::encode_wal_group_row(&crate::wal::wal_group_row(g, None))?,
    })
}

/// Encode a whole `SnapshotV3` as one scrape's rows.
///
/// A `Snapshot` that is not V3 has no acquisition groups to serve and is an
/// error rather than an empty body: silently returning nothing would look to
/// a consumer exactly like an agent whose samplers are all disabled.
#[cfg(feature = "write")]
pub fn encode_snapshot(
    snapshot: &metriken_exposition::Snapshot,
    ts: i64,
    wall_offset: i64,
) -> Result<AgentRows, String> {
    let metriken_exposition::Snapshot::V3(v3) = snapshot else {
        return Err(
            "the row format carries acquisition groups, which only a V3 snapshot has".to_string(),
        );
    };
    Ok(AgentRows {
        wall_ns: v3
            .systemtime
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
        duration_ns: v3.duration.as_nanos() as u64,
        ts,
        wall_offset,
        rows: v3
            .groups
            .iter()
            .map(encode_group)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// The `Content-Type` of the streaming subscription endpoint.
///
/// Distinct from [`CONTENT_TYPE`]: a stream body is a sequence of
/// length-prefixed frames, not one `AgentRows`, and a consumer that decoded
/// one as the other would read the first frame's length as msgpack.
pub const STREAM_CONTENT_TYPE: &str = "application/vnd.rezolus.rows.v1+msgpack-stream";

/// One frame of a subscription stream.
///
/// `seq` is the index of the **subscriber's own interval** that this frame
/// covers: the producer's wall clock at the sampling pass, divided by the
/// interval this subscription asked for.
///
/// A count of frames sent would not do: it increments by one across a skipped
/// interval, presenting a contiguous sequence with data missing from the
/// middle — an undetectable hole. An interval index says the thing worth
/// knowing, in the subscriber's own terms: *an interval I asked for produced
/// no frame*.
///
/// It advances by one per interval in the healthy case. A gap means one of two
/// things, and both are worth seeing: the agent produced no new reading for
/// that interval (which is what an interval shorter than the snapshot TTL
/// looks like — the subscriber is asking faster than the operator allows), or
/// a reading was genuinely missed.
///
/// So a consumer checks that `seq` increases by exactly one, and treats
/// anything else — a jump, or a stream that simply ends, which is
/// indistinguishable from a truncated connection — as a gap to be refilled
/// rather than as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamFrame {
    pub seq: u64,
    pub rows: AgentRows,
}

/// Encode one frame: a `u32` big-endian length, then that many bytes of
/// msgpack.
///
/// Length-prefixed rather than self-delimiting because a consumer must be able
/// to find a frame boundary without decoding — the same reason
/// [`AgentRow`]'s routing fields are in the clear.
pub fn encode_frame(rows: &AgentRows, seq: u64) -> Result<Vec<u8>, String> {
    // A tuple, not a `StreamFrame`, purely so this can borrow `rows` instead
    // of cloning a whole tick to build one. rmp-serde encodes a struct as a
    // positional array, so the two are the same bytes —
    // `a_frame_round_trips_through_its_struct` pins that.
    let payload =
        rmp_serde::to_vec(&(seq, rows)).map_err(|e| format!("failed to encode a frame: {e}"))?;
    let len = u32::try_from(payload.len()).map_err(|_| {
        format!(
            "frame of {} bytes exceeds the u32 length prefix",
            payload.len()
        )
    })?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decode one frame's payload — the bytes AFTER the length prefix.
pub fn decode_frame(payload: &[u8]) -> Result<StreamFrame, String> {
    rmp_serde::from_slice(payload).map_err(|e| format!("failed to decode a frame: {e}"))
}

/// A borrowed view of an [`AgentRow`], for encoding without copying.
///
/// rmp-serde writes a struct as a positional array, so this produces the same
/// bytes as the owned [`AgentRow`] — pinned by
/// `a_borrowed_row_encodes_exactly_like_an_owned_one`.
///
/// It exists for the streaming path. One sampling tick is shared by every
/// subscriber, but *which* schemas each still needs is per-connection, so
/// without this each subscriber would deep-clone the whole tick — payload
/// bytes included — every time, purely to null out a few schema fields.
#[derive(Serialize)]
struct AgentRowRef<'a> {
    stream: &'a str,
    window: Option<(u64, u64)>,
    schema_hash: (u64, u64),
    schema: Option<&'a GroupSchema>,
    arity: (u32, u32, u32),
    approx_bytes: u32,
    row: &'a [u8],
}

#[derive(Serialize)]
struct AgentRowsRef<'a> {
    wall_ns: u64,
    duration_ns: u64,
    ts: i64,
    wall_offset: i64,
    rows: Vec<AgentRowRef<'a>>,
}

/// What a consumer should be told about one row this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDisposition {
    /// Leave the row out. Its acquisition window has not advanced, so there is
    /// no new observation to report — see [`encode_frame_filtered`].
    Omit,
    /// Include the row, carrying its schema (this consumer has not been taught
    /// this generation).
    SendWithSchema,
    /// Include the row without its schema (already taught).
    Send,
}

/// Encode one frame, asking `decide` what to do with each row.
///
/// # An empty frame is a statement, not a wasted one
///
/// A frame with no rows says *this interval elapsed and nothing new was
/// observed*, and it is sent. That is worth about twenty bytes and it buys two
/// things that skipping it would cost:
///
/// - **`seq` stays contiguous**, so a gap means exactly one thing: a reading
///   was lost. Skipping empty frames made a gap ambiguous between "nothing had
///   changed" and "your subscription was starved", which are the two cases a
///   consumer most needs to tell apart — and it is the reason `seq` exists.
///
/// - **It is a keepalive.** Without it, a subscriber whose groups are all slow
///   cannot distinguish a quiet agent from a dead connection.
///
/// # Why a row can be left out
///
/// A snapshot carries every group the producer knows about, including ones
/// whose sampler did not read this tick — a 60 s `drivehealth` sweep sits
/// unchanged across hundreds of ticks, keeping the acquisition window of its
/// last real read (that is the point of the window; see the agent's
/// `timing.rs`). Repeating such a row would assert an observation that did not
/// happen. A recorder would drop it on the same window-advance rule it already
/// applies, but a consumer that plots what it receives — a live viewer — would
/// draw a point where there was no reading.
///
/// So the stream sends **updates**, and a consumer's view of a group persists
/// until the group is mentioned again. The window in each row says when the
/// reading it carries was actually taken, which is what makes that safe.
pub fn encode_frame_filtered(
    rows: &AgentRows,
    seq: u64,
    mut decide: impl FnMut(&AgentRow) -> RowDisposition,
) -> Result<Vec<u8>, String> {
    let kept: Vec<AgentRowRef<'_>> = rows
        .rows
        .iter()
        .filter_map(|r| match decide(r) {
            RowDisposition::Omit => None,
            disposition => Some(AgentRowRef {
                stream: &r.stream,
                window: r.window,
                schema_hash: r.schema_hash,
                schema: if disposition == RowDisposition::SendWithSchema {
                    r.schema.as_ref()
                } else {
                    None
                },
                arity: r.arity,
                approx_bytes: r.approx_bytes,
                row: &r.row,
            }),
        })
        .collect();

    let view = AgentRowsRef {
        wall_ns: rows.wall_ns,
        duration_ns: rows.duration_ns,
        ts: rows.ts,
        wall_offset: rows.wall_offset,
        rows: kept,
    };
    let payload =
        rmp_serde::to_vec(&(seq, &view)).map_err(|e| format!("failed to encode a frame: {e}"))?;
    let len = u32::try_from(payload.len()).map_err(|_| {
        format!(
            "frame of {} bytes exceeds the u32 length prefix",
            payload.len()
        )
    })?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> AgentRows {
        AgentRows {
            wall_ns: 7,
            duration_ns: 3,
            ts: 4,
            wall_offset: 3,
            rows: vec![AgentRow {
                stream: "cpu/usage".to_string(),
                window: Some((1, 2)),
                schema_hash: (9, 9),
                schema: None,
                arity: (1, 0, 0),
                approx_bytes: 16,
                row: vec![0xc0],
            }],
        }
    }

    /// `encode_frame` borrows a tuple where `decode_frame` produces a struct.
    /// That only works because rmp-serde writes a struct as a positional
    /// array; if it ever did not, frames would encode and decode differently
    /// and the mismatch would show up as corrupt data rather than an error.
    #[test]
    fn a_frame_round_trips_through_its_struct() {
        let rows = rows();
        let framed = encode_frame(&rows, 41).unwrap();
        let len = u32::from_be_bytes(framed[..4].try_into().unwrap()) as usize;
        assert_eq!(
            len,
            framed.len() - 4,
            "the prefix must describe the payload"
        );

        let frame = decode_frame(&framed[4..]).unwrap();
        assert_eq!(frame.seq, 41);
        assert_eq!(frame.rows, rows);
    }

    /// The borrowed encode view must be byte-identical to the owned type, or
    /// the streaming path and the poll path would disagree about the format
    /// while both claiming the same content type.
    #[test]
    fn a_borrowed_row_encodes_exactly_like_an_owned_one() {
        let mut rows = rows();
        rows.rows[0].schema = Some(GroupSchema::default());

        let borrowed = encode_frame_filtered(&rows, 5, |_| RowDisposition::SendWithSchema).unwrap();
        let owned = encode_frame(&rows, 5).unwrap();
        assert_eq!(borrowed, owned);

        // ...and dropping a schema through the filter must equal having built
        // the row without one.
        let dropped = encode_frame_filtered(&rows, 5, |_| RowDisposition::Send).unwrap();
        let mut without = rows.clone();
        without.rows[0].schema = None;
        assert_eq!(dropped, encode_frame(&without, 5).unwrap());
        assert_ne!(dropped, borrowed, "the filter has to actually do something");
    }

    /// Omitting a row must equal never having had it, so a consumer cannot
    /// tell a filtered frame from one the producer built that way.
    #[test]
    fn an_omitted_row_encodes_as_though_it_were_never_there() {
        let mut rows = rows();
        rows.rows.push(AgentRow {
            stream: "drivehealth/temp".to_string(),
            window: Some((5, 6)),
            schema_hash: (1, 1),
            schema: None,
            arity: (1, 0, 0),
            approx_bytes: 8,
            row: vec![0xc0],
        });

        let filtered = encode_frame_filtered(&rows, 9, |r| {
            if r.stream == "drivehealth/temp" {
                RowDisposition::Omit
            } else {
                RowDisposition::Send
            }
        })
        .unwrap();

        let mut only_first = rows.clone();
        only_first.rows.truncate(1);
        assert_eq!(filtered, encode_frame(&only_first, 9).unwrap());
    }

    /// An interval on which nothing advanced still gets a frame — an empty
    /// one. It keeps `seq` contiguous, so a gap means a lost reading and
    /// nothing else, and it doubles as a keepalive.
    #[test]
    fn an_interval_with_no_updates_still_sends_a_frame() {
        let rows = rows();
        let framed = encode_frame_filtered(&rows, 3, |_| RowDisposition::Omit).unwrap();

        let frame = decode_frame(&framed[4..]).unwrap();
        assert_eq!(frame.seq, 3, "the interval it covers is still named");
        assert!(frame.rows.rows.is_empty(), "and it carries no observations");

        // Small enough that sending one per idle interval is not a cost worth
        // trading the contiguity for.
        assert!(framed.len() < 64, "empty frame was {} bytes", framed.len());
    }

    /// A stream body is several frames back to back, and a consumer has to be
    /// able to walk them using only the prefixes.
    #[test]
    fn frames_concatenate_and_are_walkable_by_length_alone() {
        let rows = rows();
        let mut body = Vec::new();
        for seq in 0..4 {
            body.extend_from_slice(&encode_frame(&rows, seq).unwrap());
        }

        let mut at = 0usize;
        let mut seen = Vec::new();
        while at < body.len() {
            let len = u32::from_be_bytes(body[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
            seen.push(decode_frame(&body[at..at + len]).unwrap().seq);
            at += len;
        }
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    /// The replication stream carries a payload and no envelope, and the
    /// envelope rebuilt from it must be the one the row endpoint would have
    /// sent for the same group — every field, because each one drives a
    /// staging decision. Checked against `encode_group`, the row endpoint's
    /// own builder, rather than against constants.
    ///
    /// Both payload shapes the producer sends: with the schema inside (its
    /// first mention on a connection) and without (every later tick).
    #[cfg(feature = "write")]
    #[test]
    fn an_envelope_rebuilt_from_a_stream_payload_matches_the_row_endpoints() {
        use crate::wal::{encode_wal_group_row, wal_group_row};

        let desc = |name: &str| metriken_exposition::MetricDesc {
            name: name.to_string(),
            metadata: [("metric".to_string(), format!("m{name}"))]
                .into_iter()
                .collect(),
        };
        let producer_schema = metriken_exposition::GroupSchema {
            counters: vec![desc("0"), desc("1")],
            gauges: Vec::new(),
            histograms: vec![desc("2")],
        };
        let schema: crate::schema::GroupSchema = (&producer_schema).into();
        let mut h = histogram::Histogram::new(3, 8).unwrap();
        h.increment(5).unwrap();
        let g = metriken_exposition::GroupSnapshot {
            name: "cpu_usage/percpu".to_string(),
            schema_hash: producer_schema.hash(),
            schema: Some(std::sync::Arc::new(producer_schema.clone())),
            window: Some(metriken::Window::new(900, 1_000)),
            counters: vec![Some(7), None],
            gauges: Vec::new(),
            histograms: vec![Some(h)],
        };
        let from_endpoint = encode_group(&g).unwrap();

        // First mention: the schema rides inside the payload.
        let anchored = encode_wal_group_row(&wal_group_row(&g, Some(schema.clone()))).unwrap();
        let rebuilt = AgentRow::from_payload(g.name.clone(), anchored).unwrap();
        assert_eq!(rebuilt, from_endpoint, "a schema-carrying payload");
        assert_eq!(rebuilt.schema.as_ref().unwrap().hash(), rebuilt.schema_hash);

        // Every later tick: no schema in the payload, and the bytes are passed
        // through rather than re-encoded.
        let bare = encode_wal_group_row(&wal_group_row(&g, None)).unwrap();
        let rebuilt = AgentRow::from_payload(g.name.clone(), bare.clone()).unwrap();
        assert_eq!(rebuilt.row, bare, "passed through, not re-encoded");
        assert_eq!(
            AgentRow {
                schema: None,
                ..from_endpoint.clone()
            },
            rebuilt,
            "a bare payload"
        );
        assert_eq!(rebuilt.arity, (2, 0, 1));
        assert_eq!(
            rebuilt.approx_bytes as usize,
            crate::rez::group_approx_bytes(&g),
            "metered as the snapshot path meters it"
        );
    }

    /// Bytes that are not a group row are an error naming the stream, not a
    /// row with garbage in it: the stream is the one identifier a caller has
    /// to act on.
    #[test]
    fn a_payload_that_is_not_a_group_row_is_refused_by_name() {
        let err = AgentRow::from_payload("cpu/usage".to_string(), vec![0x93, 0x01, 0x02])
            .expect_err("must refuse");
        assert!(err.contains("cpu/usage"), "{err}");
    }
}
