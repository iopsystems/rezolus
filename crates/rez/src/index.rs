//! The identity index: what a group's slots mean, and when that changed.
//!
//! A group's values are positional — slot `v3` is the fourth member of
//! `cpu_usage/cpu_usage_task`. What that slot *is* (which task, which cgroup,
//! which CPU) lives here rather than in the column metadata a schema carries,
//! for two reasons.
//!
//! The first is that column metadata cannot express change. dendro's FORMAT.md
//! §8 says so directly: "A column means one thing for the life of a stream. A
//! fact that changes over time belongs in `caller_rows` (§3.5), keyed by the
//! time it changed, never in field metadata." A slot that held task A and now
//! holds task B is exactly that fact.
//!
//! The second is measured. Because identity lives in the schema today, a single
//! task exiting changes the schema hash and re-sends every descriptor in the
//! group. On `delta`, `cpu_usage/cpu_usage_task` changed on 59 of 60 scrapes
//! and carries 638 descriptors, and two frames with the *same* 49 rows measured
//! 45,350 B and 158,360 B depending only on whether schemas were re-sent. Under
//! this format that event is one [`SlotEntry`].
//!
//! # What dendro stores, and why `kind` and `state` are in the blob
//!
//! dendro stores `caller_rows` as `(source_id, stream, ts, blob)` and never
//! decodes the blob, so this format is ours. Its `Frame::Index` does carry
//! `kind` and `state` as frame fields — but its `Subscriber` writes only
//! `CallerRow { ts, blob }`, using those two to check the live stream and then
//! dropping them. An archive read back later therefore sees blobs and nothing
//! else, so an entry that did not carry its own `kind` and `state` could not be
//! replayed from storage at all — only followed live. They are duplicated
//! deliberately: the frame's copy serves the subscriber, the blob's copy serves
//! the archive.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The hash of a complete slot set, as `(hi, lo)`.
///
/// Structurally identical to `dendro::replicate::IndexState`, and a value of
/// this type is what goes in a `Frame::Index`'s `state` and a `Frame::Rows`'s
/// `index_state`.
pub type IndexState = (u64, u64);

/// Whether an entry carries every live slot or only what changed.
///
/// Mirrors `dendro::replicate::IndexKind` for the same reason
/// [`GroupSchema`](crate::schema::GroupSchema) mirrors the producer's type:
/// this one is serialized into a blob that outlives the process, so its
/// encoding is ours to keep stable. rmp-serde writes a fieldless enum as its
/// variant index, so a variant inserted before `Full` would silently change
/// what already-written archives mean. `kind_agrees_with_dendros` pins the
/// correspondence in both directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// Every live slot. Sent on connect, so a subscriber starts complete, and
    /// re-sent at the seal cadence, because `caller_rows` is evicted on the
    /// same cutoff as segments — state written once at t0 is deleted while
    /// later rows still reference it.
    Full,
    /// Only slots added or whose labels changed, plus the slots cleared.
    /// Meaningless without a `Full` before it.
    Delta,
}

impl From<EntryKind> for dendro::replicate::IndexKind {
    fn from(k: EntryKind) -> Self {
        match k {
            EntryKind::Full => dendro::replicate::IndexKind::Full,
            EntryKind::Delta => dendro::replicate::IndexKind::Delta,
        }
    }
}

impl From<dendro::replicate::IndexKind> for EntryKind {
    fn from(k: dendro::replicate::IndexKind) -> Self {
        match k {
            dendro::replicate::IndexKind::Full => EntryKind::Full,
            dendro::replicate::IndexKind::Delta => EntryKind::Delta,
        }
    }
}

/// What one slot means.
///
/// `slot` is the real member index — the CPU id, the BPF map slot — not a rank.
/// A removal can therefore name a slot, and a slot's identity does not move
/// when a neighbour appears or goes away. Where a value sits in a row's values
/// vector is the rank of its slot in the sorted live set, which is derived
/// rather than stored.
///
/// `labels` is an open map because what identifies a slot differs by group:
/// `cpu=11`, `name=/foo.service`, `device=card0`. A new sampler with a new kind
/// of identity needs no format change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub slot: u32,
    pub labels: BTreeMap<String, String>,
}

/// One entry for one stream at one timestamp: the blob of a `Frame::Index`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub kind: EntryKind,
    /// `Full`: every live slot. `Delta`: those added or changed.
    pub slots: Vec<SlotEntry>,
    /// `Delta` only, and always empty on a `Full` — a `Full` states the whole
    /// set, so anything absent from `slots` is gone by construction.
    pub removed: Vec<u32>,
    /// The hash of the COMPLETE slot set after applying this entry, not of the
    /// change. A consumer applies the entry and compares its own accumulated
    /// set against this, which makes a missed delta loud rather than silent.
    pub state: IndexState,
}

impl IndexEntry {
    /// msgpack, the encoding dendro's `caller_rows` blob holds.
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec(self).expect("IndexEntry serialization is infallible")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        rmp_serde::from_slice(bytes)
    }
}

/// Why an entry could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The accumulated set does not hash to what the entry says it should.
    /// Something was lost, reordered, or applied twice; the rows that name this
    /// state must be skipped rather than attributed to the wrong slots.
    StateMismatch {
        expected: IndexState,
        actual: IndexState,
    },
    /// A `Delta` arrived before any `Full`. There is no base to apply it to,
    /// and an empty index is not one — it is indistinguishable from a `Full`
    /// that legitimately had no live slots.
    DeltaBeforeFull,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateMismatch { expected, actual } => write!(
                f,
                "index state mismatch: entry says {expected:016x?}, accumulated set hashes to {actual:016x?}"
            ),
            Self::DeltaBeforeFull => write!(f, "delta entry with no full entry before it"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// The live slot set of one stream.
///
/// Both halves use it. A producer calls [`observe`](Self::observe) each tick
/// and transmits what comes back; a consumer calls [`apply`](Self::apply) on
/// what it receives. They are the same type on purpose — the producer's copy is
/// what the consumer's is checked against, and a state hash computed by two
/// different implementations would be a second thing to keep in agreement.
#[derive(Clone, Debug, Default)]
pub struct SlotIndex {
    live: BTreeMap<u32, BTreeMap<String, String>>,
    seen_full: bool,
}

impl SlotIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn labels(&self, slot: u32) -> Option<&BTreeMap<String, String>> {
        self.live.get(&slot)
    }

    /// The slots in the order a row's values vector uses: sorted, so a value's
    /// position is its slot's rank.
    pub fn slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.live.keys().copied()
    }

    /// FNV-1a-128 over the canonical msgpack encoding of the live set.
    ///
    /// Deterministic because both maps are `BTreeMap`s. The same function
    /// [`GroupSchema::hash`](crate::schema::GroupSchema::hash) uses, over a
    /// different domain.
    pub fn state(&self) -> IndexState {
        let bytes = rmp_serde::to_vec(&self.live).expect("slot set serialization is infallible");
        crate::schema::fnv1a_128(&bytes)
    }

    /// Producer side: fold this tick's complete live set in, and return what to
    /// transmit — or `None` when nothing moved, which is the common case.
    ///
    /// `observed` is the whole live set, not a change: the caller walks the
    /// group's populated slots and this works out what is new. Returning `None`
    /// for an unchanged set is what keeps a steady group silent between
    /// [`full_entry`](Self::full_entry) resends.
    pub fn observe<I>(&mut self, observed: I) -> Option<IndexEntry>
    where
        I: IntoIterator<Item = (u32, BTreeMap<String, String>)>,
    {
        let next: BTreeMap<u32, BTreeMap<String, String>> = observed.into_iter().collect();

        let mut slots = Vec::new();
        for (&slot, labels) in &next {
            // A slot whose labels changed is an update, not a removal followed
            // by an addition: it is the recycled-identity case, and the slot
            // never stopped being live.
            if self.live.get(&slot) != Some(labels) {
                slots.push(SlotEntry {
                    slot,
                    labels: labels.clone(),
                });
            }
        }
        let removed: Vec<u32> = self
            .live
            .keys()
            .filter(|slot| !next.contains_key(slot))
            .copied()
            .collect();

        let first = !self.seen_full;
        if slots.is_empty() && removed.is_empty() && !first {
            return None;
        }

        self.live = next;
        self.seen_full = true;

        // The first entry of a stream is a `Full` whatever it contains, so a
        // consumer has a base before any delta reaches it — including the empty
        // case, where a `Delta` carrying nothing would be indistinguishable
        // from no entry at all.
        if first {
            return Some(self.full_entry());
        }

        Some(IndexEntry {
            kind: EntryKind::Delta,
            slots,
            removed,
            state: self.state(),
        })
    }

    /// The complete state, for connect-time completeness and the seal-cadence
    /// resend that keeps eviction from orphaning identity.
    pub fn full_entry(&self) -> IndexEntry {
        IndexEntry {
            kind: EntryKind::Full,
            slots: self
                .live
                .iter()
                .map(|(&slot, labels)| SlotEntry {
                    slot,
                    labels: labels.clone(),
                })
                .collect(),
            removed: Vec::new(),
            state: self.state(),
        }
    }

    /// Consumer side: apply a received entry, refusing it if the result does
    /// not hash to what the entry claims.
    ///
    /// On refusal the index is left as it was, so a caller that skips the
    /// offending rows and waits for the next `Full` recovers rather than
    /// carrying a half-applied set forward.
    pub fn apply(&mut self, entry: &IndexEntry) -> Result<(), ApplyError> {
        if entry.kind == EntryKind::Delta && !self.seen_full {
            return Err(ApplyError::DeltaBeforeFull);
        }

        let mut next = match entry.kind {
            EntryKind::Full => BTreeMap::new(),
            EntryKind::Delta => self.live.clone(),
        };
        for slot in &entry.removed {
            next.remove(slot);
        }
        for e in &entry.slots {
            next.insert(e.slot, e.labels.clone());
        }

        let candidate = Self {
            live: next,
            seen_full: true,
        };
        let actual = candidate.state();
        if actual != entry.state {
            return Err(ApplyError::StateMismatch {
                expected: entry.state,
                actual,
            });
        }

        *self = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn task(slot: u32, comm: &str) -> (u32, BTreeMap<String, String>) {
        (slot, labels(&[("comm", comm), ("cgroup", "/system.slice")]))
    }

    /// `EntryKind` is written into a blob that outlives the process while
    /// `IndexKind` travels on the wire, and a `Frame::Index` carries both — so
    /// they have to mean the same thing. rmp-serde encodes a fieldless enum as
    /// its variant index, so a variant added to either in the wrong position
    /// would not fail to compile.
    #[test]
    fn kind_agrees_with_dendros() {
        use dendro::replicate::IndexKind;
        for (ours, theirs) in [
            (EntryKind::Full, IndexKind::Full),
            (EntryKind::Delta, IndexKind::Delta),
        ] {
            assert_eq!(IndexKind::from(ours), theirs);
            assert_eq!(EntryKind::from(theirs), ours);
        }
    }

    /// The property the whole format exists for: a consumer that applies every
    /// entry ends up holding exactly what the producer holds, having been sent
    /// only the changes.
    #[test]
    fn a_consumer_following_deltas_ends_up_where_the_producer_is() {
        let mut producer = SlotIndex::new();
        let mut consumer = SlotIndex::new();

        let ticks = vec![
            vec![task(0, "redis"), task(1, "nginx")],
            vec![task(0, "redis"), task(1, "nginx"), task(2, "cron")],
            // 1 exits, and 0 is recycled onto a different task.
            vec![task(0, "valkey"), task(2, "cron")],
            vec![task(0, "valkey"), task(2, "cron")],
        ];

        let mut sent = 0usize;
        for tick in ticks {
            if let Some(entry) = producer.observe(tick) {
                consumer.apply(&entry).expect("applies");
                sent += 1;
            }
        }

        assert_eq!(sent, 3, "the fourth tick changed nothing and sent nothing");
        assert_eq!(consumer.state(), producer.state());
        assert_eq!(consumer.len(), 2);
        assert_eq!(
            consumer.labels(0).unwrap().get("comm").map(String::as_str),
            Some("valkey"),
        );
        assert!(consumer.labels(1).is_none(), "slot 1 was removed");
    }

    /// Rule 10: a consumer whose accumulated state does not hash to what the
    /// entry claims must refuse it. Misattribution — one task's numbers under
    /// another's name — is worse than a gap.
    #[test]
    fn a_skipped_entry_is_refused_rather_than_misattributed() {
        let mut producer = SlotIndex::new();
        let mut consumer = SlotIndex::new();

        let first = producer.observe(vec![task(0, "redis")]).unwrap();
        consumer.apply(&first).expect("base applies");

        // The consumer never sees this one, and nothing later undoes it.
        let _lost = producer.observe(vec![task(0, "redis"), task(1, "nginx")]);
        let next = producer
            .observe(vec![task(0, "valkey"), task(1, "nginx")])
            .unwrap();

        let before = consumer.state();
        let err = consumer.apply(&next).expect_err("must refuse");
        assert!(matches!(err, ApplyError::StateMismatch { .. }), "{err:?}");
        assert_eq!(consumer.state(), before, "a refused entry changes nothing");
    }

    /// The other side of hashing the result rather than the change: a lost
    /// entry whose effect a later one undoes leaves the consumer holding the
    /// right set, and it is then not an error. Hashing the delta would have
    /// made this a permanent desync over a difference that no longer exists.
    #[test]
    fn a_loss_a_later_entry_undoes_is_not_an_error() {
        let mut producer = SlotIndex::new();
        let mut consumer = SlotIndex::new();
        consumer
            .apply(&producer.observe(vec![task(0, "redis")]).unwrap())
            .unwrap();

        // Slot 1 appears and goes away again; the consumer misses both halves
        // of that, so what it holds is still correct.
        let _lost = producer.observe(vec![task(0, "redis"), task(1, "nginx")]);
        let next = producer
            .observe(vec![task(0, "redis"), task(2, "cron")])
            .unwrap();

        consumer
            .apply(&next)
            .expect("the set is right, so it applies");
        assert_eq!(consumer.state(), producer.state());
    }

    /// A `Full` is what recovers from that: it states the whole set, so it
    /// applies to a consumer no matter how far behind it had fallen.
    #[test]
    fn a_full_recovers_a_consumer_that_fell_behind() {
        let mut producer = SlotIndex::new();
        let mut consumer = SlotIndex::new();
        consumer
            .apply(&producer.observe(vec![task(0, "redis")]).unwrap())
            .unwrap();

        let _lost = producer.observe(vec![task(5, "nginx"), task(9, "cron")]);

        consumer
            .apply(&producer.full_entry())
            .expect("full applies");
        assert_eq!(consumer.state(), producer.state());
        assert!(consumer.labels(0).is_none());
    }

    /// The `cpu_usage_task` case: a slot whose task exited and was reused is
    /// one changed slot, not a removal and an addition. `removed` naming it
    /// would make a consumer drop identity it is about to be given back.
    #[test]
    fn a_recycled_slot_is_an_update_not_a_removal() {
        let mut producer = SlotIndex::new();
        producer.observe(vec![task(3, "old_task")]).unwrap();

        let entry = producer.observe(vec![task(3, "new_task")]).unwrap();
        assert_eq!(entry.kind, EntryKind::Delta);
        assert_eq!(entry.slots.len(), 1);
        assert_eq!(entry.slots[0].slot, 3);
        assert_eq!(entry.slots[0].labels.get("comm").unwrap(), "new_task");
        assert!(
            entry.removed.is_empty(),
            "the slot never stopped being live"
        );
    }

    /// A removal has to be carried, not implied. A `Delta` whose `removed` was
    /// dropped would leave the consumer holding a dead task, and the state
    /// check is what turns that into a refusal instead of silence.
    #[test]
    fn a_removal_travels_in_the_entry() {
        let mut producer = SlotIndex::new();
        producer
            .observe(vec![task(0, "redis"), task(1, "nginx")])
            .unwrap();
        let entry = producer.observe(vec![task(0, "redis")]).unwrap();

        assert_eq!(entry.removed, vec![1]);
        assert!(entry.slots.is_empty(), "nothing was added or changed");

        let mut consumer = SlotIndex::new();
        consumer.apply(&producer.full_entry()).unwrap();
        assert_eq!(consumer.len(), 1);
    }

    /// The first entry of a stream is a `Full` whatever it holds, including
    /// nothing. An empty `Delta` would be indistinguishable from no entry, and
    /// a consumer would have no base to apply the next one to.
    #[test]
    fn the_first_entry_is_full_even_when_empty() {
        let mut producer = SlotIndex::new();
        let first = producer
            .observe(Vec::new())
            .expect("an opening entry is sent");
        assert_eq!(first.kind, EntryKind::Full);
        assert!(first.slots.is_empty());

        let mut consumer = SlotIndex::new();
        consumer.apply(&first).expect("applies");
        let second = producer.observe(vec![task(0, "redis")]).unwrap();
        assert_eq!(second.kind, EntryKind::Delta);
        consumer
            .apply(&second)
            .expect("and so does the delta after it");
    }

    #[test]
    fn a_delta_before_a_full_is_refused() {
        let mut producer = SlotIndex::new();
        producer.observe(Vec::new()).unwrap();
        let delta = producer.observe(vec![task(0, "redis")]).unwrap();

        let mut fresh = SlotIndex::new();
        assert_eq!(
            fresh.apply(&delta).expect_err("no base"),
            ApplyError::DeltaBeforeFull,
        );
    }

    /// The seal-cadence resend must not look like a change to anything
    /// downstream: same set, same state, so rows built against it stay valid.
    #[test]
    fn a_full_resend_does_not_move_the_state() {
        let mut producer = SlotIndex::new();
        producer
            .observe(vec![task(0, "redis"), task(7, "nginx")])
            .unwrap();
        let before = producer.state();

        let full = producer.full_entry();
        assert_eq!(full.state, before);

        let mut consumer = SlotIndex::new();
        consumer.apply(&full).unwrap();
        consumer
            .apply(&full)
            .expect("applying it twice is idempotent");
        assert_eq!(consumer.state(), before);
    }

    #[test]
    fn an_entry_round_trips_through_its_blob_encoding() {
        let mut producer = SlotIndex::new();
        producer.observe(vec![task(0, "redis")]).unwrap();
        let entry = producer
            .observe(vec![task(0, "redis"), task(4, "nginx")])
            .unwrap();

        let decoded = IndexEntry::decode(&entry.encode()).expect("decodes");
        assert_eq!(decoded, entry);
    }

    /// Slot order is the rank order a row's values vector uses, so it has to be
    /// sorted by slot rather than by arrival.
    #[test]
    fn slots_come_back_in_rank_order() {
        let mut producer = SlotIndex::new();
        producer
            .observe(vec![task(9, "c"), task(2, "a"), task(5, "b")])
            .unwrap();
        assert_eq!(producer.slots().collect::<Vec<_>>(), vec![2, 5, 9]);
    }

    /// What Phase 2 of #1224 is for, in bytes.
    ///
    /// `cpu_usage/cpu_usage_task` on `delta` carries 638 descriptors and
    /// changed on 59 of 60 scrapes, because one task exiting changes the schema
    /// hash and re-sends the whole thing. This builds that group both ways and
    /// compares what one such tick costs.
    #[test]
    fn one_task_churning_costs_a_schema_today_and_an_entry_under_this_format() {
        const TASKS: u32 = 319;

        let live = |generation: u32| -> Vec<(u32, BTreeMap<String, String>)> {
            (0..TASKS)
                .map(|slot| {
                    // Only slot 0 differs between generations: one task exited
                    // and its slot was reused, which is the measured case.
                    let comm = if slot == 0 && generation == 1 {
                        "new_task".to_string()
                    } else {
                        format!("task_{slot}")
                    };
                    (
                        slot,
                        labels(&[
                            ("comm", comm.as_str()),
                            ("cgroup", "/system.slice/redis.service"),
                        ]),
                    )
                })
                .collect()
        };

        // Today: identity lives in per-column metadata, so the tick re-sends
        // every descriptor of both metrics.
        let schema = crate::schema::GroupSchema {
            counters: live(1)
                .into_iter()
                .flat_map(|(slot, l)| {
                    [0usize, 1].into_iter().map(move |metric_id| {
                        let mut metadata = l.clone();
                        metadata.insert("id".to_string(), slot.to_string());
                        metadata.insert(
                            "metric".to_string(),
                            if metric_id == 0 {
                                "cpu_usage_user".to_string()
                            } else {
                                "cpu_usage_system".to_string()
                            },
                        );
                        crate::schema::MetricDesc {
                            name: format!("{metric_id}x{slot}"),
                            metadata,
                        }
                    })
                })
                .collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let schema_bytes = rmp_serde::to_vec(&schema).unwrap().len();
        assert_eq!(schema.counters.len(), 638, "the measured descriptor count");

        let mut producer = SlotIndex::new();
        let full_bytes = producer.observe(live(0)).unwrap().encode().len();
        let delta_bytes = producer.observe(live(1)).unwrap().encode().len();

        println!(
            "\n  {TASKS} tasks, 638 descriptors, one slot recycled\n\
               \x20   schema re-sent (today): {schema_bytes:>7} B\n\
               \x20   index Full  (on connect/seal): {full_bytes:>7} B\n\
               \x20   index Delta (the churning tick): {delta_bytes:>7} B\n"
        );

        // The claim Phase 2 rests on: the per-tick cost of task churn stops
        // scaling with the group's size. A generous bound rather than a tight
        // one — this guards the shape of the result, not the exact encoding.
        assert!(
            delta_bytes * 100 < schema_bytes,
            "a one-slot delta ({delta_bytes} B) should be under 1% of the re-sent \
             schema ({schema_bytes} B)"
        );
        assert!(
            full_bytes < schema_bytes / 2,
            "even a full resend ({full_bytes} B) is smaller than the schema \
             ({schema_bytes} B), since identity is stated once per slot rather \
             than once per slot per metric"
        );
    }
}
