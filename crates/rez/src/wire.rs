//! The row-format wire: what an agent serves to a consumer that wants WAL
//! rows rather than a snapshot.
//!
//! # Why this exists
//!
//! Not to save an encode. That was the original argument and measurement
//! does not support it: moving the same payload over this wire instead of a
//! snapshot is worth about 1.2x on the recorder and slightly NEGATIVE on
//! total CPU, because it trades one large encode for many small ones.
//!
//! What it is actually for is the schema. An agent serving `/metrics/binary`
//! ships every acquisition group's full `GroupSchema` — every member's name
//! and metadata `BTreeMap` — on **every scrape**, because it has to: the
//! exporter, the parquet recorder and the live viewer all decode one snapshot
//! in isolation and hold no cache to resolve a schema reference against. A
//! schema that repeats unchanged for hours is re-encoded, re-transmitted and
//! re-decoded every tick, and it dwarfs the values it describes — the numbers
//! are in `docs/journal/`, but the shape of them is that the schema is most
//! of the body and most of the recorder's per-tick cost.
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

/// One scrape's worth of rows: the row-format equivalent of a `SnapshotV3`.
///
/// `wall_ns` and `duration_ns` mirror `SnapshotV3`'s `systemtime`/`duration`,
/// which a consumer needs for the same reasons it does there — clock
/// reconciliation and the sampling-latency record. The rows themselves carry
/// no timestamp: a WAL row's `ts` is the CONSUMER's monotonic stamp for the
/// tick (the `wal` table's key), never the producer's, exactly as on the
/// snapshot path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRows {
    /// The producer's wall clock when the scrape was taken, in nanoseconds
    /// since the Unix epoch.
    pub wall_ns: u64,
    /// How long the producer's sampling pass took, in nanoseconds.
    pub duration_ns: u64,
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
pub fn encode_snapshot(snapshot: &metriken_exposition::Snapshot) -> Result<AgentRows, String> {
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
        rows: v3
            .groups
            .iter()
            .map(encode_group)
            .collect::<Result<Vec<_>, _>>()?,
    })
}
