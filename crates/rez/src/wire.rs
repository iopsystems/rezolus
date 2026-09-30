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

use std::sync::Arc;

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
    ///
    /// Shared with the producer's [`SchemaCache`], which converts a group's
    /// schema once per hash rather than once per pass.
    ///
    /// [`SchemaCache`]: metriken_archive::stream::SchemaCache
    pub schema: Option<Arc<GroupSchema>>,
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
                (Some(Arc::new(schema)), row)
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

/// An agent's row, read by metriken-archive's stream `FrameProducer`
/// directly. The agent builds these once per pass for `/metrics/rows`, and
/// its stream sends the same rows, so a stream frame costs no conversion.
#[cfg(feature = "write")]
impl metriken_archive::stream::StreamRow for AgentRow {
    fn stream(&self) -> &str {
        &self.stream
    }

    fn schema_hash(&self) -> (u64, u64) {
        self.schema_hash
    }

    fn schema(&self) -> Option<&GroupSchema> {
        self.schema.as_deref()
    }

    fn payload(&self) -> &[u8] {
        &self.row
    }
}

/// Encode one producer-side acquisition group as an [`AgentRow`].
///
/// The row's `schema` is the group's schema from `schemas`, converted only
/// when its hash changed since the producer's last pass. The payload's schema
/// is always `None`; see the module docs.
#[cfg(feature = "write")]
pub fn encode_group(
    g: &metriken_exposition::GroupSnapshot,
    schemas: &mut metriken_archive::stream::SchemaCache,
) -> Result<AgentRow, String> {
    Ok(AgentRow {
        stream: g.name.clone(),
        window: g.window.map(|w| (w.begin_ns, w.end_ns)),
        schema_hash: g.schema_hash,
        schema: schemas.schema(g),
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
    schemas: &mut metriken_archive::stream::SchemaCache,
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
            .map(|g| encode_group(g, schemas))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let from_endpoint =
            encode_group(&g, &mut metriken_archive::stream::SchemaCache::new()).unwrap();

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
