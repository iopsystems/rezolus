//! The identity index as older `.rez` archives store it: what a group's slots
//! meant, and when that changed.
//!
//! `record --stream -o out.rez` wrote one [`IndexEntry`] per stream per change
//! into the archive's `caller_rows`, keyed by the time of the change. A
//! group's values are positional, and the entries say which occupant each
//! slot held at each moment. Rezolus 6.0 no longer writes them (`--stream`
//! records `.dendro` only, which takes identity from each group's schema), but
//! archives written by 5.x carry them, and the reader replays them through
//! [`crate::indexed`]. What remains here is the blob format and its decoding.
//!
//! # Why `kind` and `state` are in the blob
//!
//! dendro stores `caller_rows` as `(source_id, stream, ts, blob)` and never
//! decodes the blob, so this format is ours. An archive read back sees blobs
//! and nothing else, so an entry has to carry its own `kind` for a reader to
//! know where a replay can start. `state` was checked by the live subscriber
//! and is kept so existing blobs still decode.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The hash of a complete slot set, as `(hi, lo)`. Stored in every entry;
/// nothing reads it back.
pub type IndexState = (u64, u64);

/// Whether an entry carries every live slot or only what changed.
///
/// Serialized into a blob that outlives the process, so its encoding is fixed:
/// rmp-serde writes a unit variant as its name, and renaming one would stop
/// already-written archives decoding. `kind_encodes_as_its_name` pins it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// Every live slot. A replay can start here.
    Full,
    /// Only slots added or whose labels changed, plus the slots cleared.
    /// Meaningless without a `Full` before it.
    Delta,
}

/// What one slot means.
///
/// `slot` is the real member index — the CPU id, the BPF map slot — not a rank.
/// `labels` is an open map because what identifies a slot differs by group:
/// `cpu=11`, `name=/foo.service`, `device=card0`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub slot: u32,
    pub labels: BTreeMap<String, String>,
}

/// One entry for one stream at one timestamp: the blob in `caller_rows`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub kind: EntryKind,
    /// `Full`: every live slot. `Delta`: those added or changed.
    pub slots: Vec<SlotEntry>,
    /// `Delta` only, and always empty on a `Full` — a `Full` states the whole
    /// set, so anything absent from `slots` is gone by construction.
    pub removed: Vec<u32>,
    /// The hash of the producer's complete index state after this entry.
    pub state: IndexState,
}

impl IndexEntry {
    /// msgpack, the encoding the `caller_rows` blob holds. Used by tests that
    /// build archives in the shape 5.x wrote.
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("IndexEntry serialization is infallible")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_round_trips_through_its_blob_encoding() {
        let entry = IndexEntry {
            kind: EntryKind::Delta,
            slots: vec![SlotEntry {
                slot: 4,
                labels: [("comm".to_string(), "nginx".to_string())]
                    .into_iter()
                    .collect(),
            }],
            removed: vec![2],
            state: (1, 2),
        };
        let decoded = IndexEntry::decode(&entry.encode()).expect("decodes");
        assert_eq!(decoded, entry);
    }

    /// Archives already on disk hold these bytes. A renamed variant would
    /// compile and stop them decoding.
    #[test]
    fn kind_encodes_as_its_name() {
        assert_eq!(rmp_serde::to_vec(&EntryKind::Full).unwrap(), b"\xa4Full");
        assert_eq!(rmp_serde::to_vec(&EntryKind::Delta).unwrap(), b"\xa5Delta");
    }
}
