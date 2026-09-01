//! The membership of one acquisition group, as the archive stores it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One member of an acquisition group: its name and the producer's metadata.
///
/// A structural mirror of `metriken_exposition::MetricDesc`, and the field
/// order is load-bearing: a WAL row carries a `GroupSchema` as msgpack, and
/// rmp-serde encodes a struct as a positional array, so the on-disk bytes ARE
/// this declaration order. `mirrors_the_producers_encoding_byte_for_byte`
/// pins that against the producer's type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricDesc {
    pub name: String,
    pub metadata: BTreeMap<String, String>,
}

/// Descriptors for every counter, gauge and histogram slot of one group, in
/// the order its value arrays use.
///
/// Mirrors `metriken_exposition::GroupSchema` for the reason [`MetricDesc`]
/// does, plus one of its own: a group table's live WAL tail cannot be
/// materialized without decoding this, so having it be the producer's type
/// put `metriken-exposition` — and through it `metriken-core`'s `linkme`
/// distributed slice, which has no wasm32 implementation — on the archive's
/// READ path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSchema {
    pub counters: Vec<MetricDesc>,
    pub gauges: Vec<MetricDesc>,
    pub histograms: Vec<MetricDesc>,
}

impl GroupSchema {
    /// FNV-1a-128 over the schema's canonical msgpack encoding, as `(hi, lo)`.
    ///
    /// The same function the producer computes, because the value is compared
    /// against the producer's: a WAL row carries `schema_hash` so a decoder
    /// can tell schema drift from steady state, and a hash that disagreed
    /// with the one written would make every row look like drift.
    /// Deterministic because `MetricDesc::metadata` is a `BTreeMap`.
    pub fn hash(&self) -> (u64, u64) {
        const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
        const PRIME: u128 = 0x0000000001000000000000000000013b;
        let bytes = rmp_serde::to_vec(self).expect("GroupSchema serialization is infallible");
        let mut h = OFFSET;
        for &b in &bytes {
            h ^= b as u128;
            h = h.wrapping_mul(PRIME);
        }
        ((h >> 64) as u64, h as u64)
    }
}

#[cfg(feature = "write")]
impl From<&metriken_exposition::MetricDesc> for MetricDesc {
    fn from(d: &metriken_exposition::MetricDesc) -> Self {
        Self {
            name: d.name.clone(),
            metadata: d.metadata.clone(),
        }
    }
}

#[cfg(feature = "write")]
impl From<&metriken_exposition::GroupSchema> for GroupSchema {
    fn from(s: &metriken_exposition::GroupSchema) -> Self {
        Self {
            counters: s.counters.iter().map(Into::into).collect(),
            gauges: s.gauges.iter().map(Into::into).collect(),
            histograms: s.histograms.iter().map(Into::into).collect(),
        }
    }
}

#[cfg(all(test, feature = "write"))]
mod tests {
    use super::*;

    fn producer_schema() -> metriken_exposition::GroupSchema {
        let desc = |n: &str, k: &str, v: &str| metriken_exposition::MetricDesc {
            name: n.to_string(),
            metadata: [(k.to_string(), v.to_string())].into_iter().collect(),
        };
        metriken_exposition::GroupSchema {
            counters: vec![desc("0", "metric", "cpu_cycles"), desc("1", "cpu", "3")],
            gauges: vec![desc("2", "metric", "cpu_freq")],
            histograms: vec![desc("3x0", "metric", "runqueue_latency")],
        }
    }

    /// The WAL blob is msgpack of a struct that CONTAINS a schema, and
    /// rmp-serde writes a struct as a positional array — so a field reordered
    /// or added here does not fail to compile, it silently writes a WAL a
    /// released reader decodes into the wrong fields. This is the guard.
    #[test]
    fn mirrors_the_producers_encoding_byte_for_byte() {
        let theirs = producer_schema();
        let ours: GroupSchema = (&theirs).into();
        assert_eq!(
            rmp_serde::to_vec(&ours).unwrap(),
            rmp_serde::to_vec(&theirs).unwrap(),
        );
    }

    /// And the hash the two compute must agree, because a WAL row's
    /// `schema_hash` is written by one and compared by the other: a
    /// disagreement makes every row look like schema drift, and
    /// `materialize_group_wal_tail` then skips rows it cannot anchor.
    #[test]
    fn hashes_the_same_as_the_producer() {
        let theirs = producer_schema();
        let ours: GroupSchema = (&theirs).into();
        assert_eq!(ours.hash(), theirs.hash());
    }

    /// An empty schema is a real case (a group registered with no members
    /// yet) and hashes to the same thing on both sides too.
    #[test]
    fn an_empty_schema_agrees_as_well() {
        let theirs = metriken_exposition::GroupSchema::default();
        let ours: GroupSchema = (&theirs).into();
        assert_eq!(ours.hash(), theirs.hash());
        assert_eq!(
            rmp_serde::to_vec(&ours).unwrap(),
            rmp_serde::to_vec(&theirs).unwrap(),
        );
    }
}
