//! What a slot means, written where it changes.
//!
//! A group's values are positional, and what slot `v3` *is* — which task,
//! which cgroup — changes while the agent runs. The labels are written onto
//! the group's metrics at the moment the slot changes hands (a `task_info`
//! event arriving, a `task_exit` clearing a slot), and the snapshot builder
//! reads them from there into each group's schema. A consumer takes identity
//! from the schema; this module keeps no copy and sends nothing.
//!
//! Each assignment is stamped with a `__uid__` ([`UID_LABEL`]) minted from a
//! per-process generation, so two occupants of one slot with identical labels
//! are still two series.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts assignments, so each one mints a distinct uid.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The label that names one occupant of a slot: `__uid__`.
///
/// A slot's labels say what it means — `comm=redis pid=4112` — and they can
/// be the same for two different things: a PID wraps, a cgroup is deleted and
/// recreated at the same path, a task restarts under the same name. A reader
/// keying series on labels alone would fuse the two into one series with a
/// reset in the middle, attributing one task's counter to another. The uid is
/// what tells them apart. It is minted here, at the assignment, once per
/// [`SlotIdentity::set`], and travels with the labels everywhere they go: the
/// snapshot's descriptors and an archive column's metadata. Two recorders
/// watching one agent therefore see the same uid for the same occupant
/// without coordinating.
///
/// Internal under the `__` rule (see `docs/labels.md`): part of series
/// identity and matchable in a selector, dropped by aggregation, hidden by
/// every listing and legend.
pub const UID_LABEL: &str = "__uid__";

/// A uid for the assignment that took `generation`.
///
/// Unique within this process because the generation is, and across
/// processes because the producer epoch is folded in: a recording that
/// spans an agent restart, or an archive combined from two, cannot see the
/// same uid twice. Sixteen hex characters, so the label is short in a
/// descriptor that is repeated per slot.
fn mint_uid(generation: u64) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in crate::agent::epoch::producer_epoch()
        .as_bytes()
        .iter()
        .chain(generation.to_le_bytes().iter())
    {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Take the next generation. Every assignment takes exactly one.
fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

/// One acquisition group and the metrics that carry its slots.
pub type GroupMetrics = (
    &'static crate::agent::timing::AcquisitionGroup,
    &'static [&'static dyn crate::agent::metrics::GroupMetadata],
);

/// The one way to write a slot's identity.
///
/// Holds the groups a slot id spans, each paired with the metrics that carry
/// it. SEVERAL groups, because one id routinely spans them: a cgroup id
/// written by `scheduler_runqueue` reaches `..._cgroup_context_switch`,
/// `..._cgroup_offcpu` and `..._cgroup_wait`, and each group's schema carries
/// its own copy of the labels.
///
/// It is the only holder of [`GroupMetadata`], so nothing writes identity
/// through that trait without a uid. metriken's group types keep their own
/// inherent `insert_metadata`, so a sampler holding a concrete `&CounterGroup`
/// can still write labels without one and the compiler will not stop it.
///
/// [`GroupMetadata`]: crate::agent::metrics::GroupMetadata
pub struct SlotIdentity {
    groups: &'static [GroupMetrics],
    /// What each live slot currently means and the uid it was given, so a
    /// re-announcement can be told from a reassignment.
    ///
    /// Samplers re-set slots they already hold: `drivehealth` sets every
    /// drive's labels on every sweep, `ethtool` every interface's on every
    /// refresh. Those are the same occupant saying its name again, and a uid
    /// minted per call would split every such series at every refresh. A new
    /// uid is minted only when the slot was not live — never set, or cleared
    /// since — or when its labels changed. A `BTreeMap` because it is built
    /// in a `const fn`; the map is small (one entry per live slot) and is
    /// touched once per assignment event, not per tick.
    live: std::sync::Mutex<Occupants>,
}

/// Per slot: the labels it was last set to (without the uid) and the uid.
type Occupants = BTreeMap<usize, (BTreeMap<String, String>, String)>;

impl SlotIdentity {
    pub const fn new(groups: &'static [GroupMetrics]) -> Self {
        Self {
            groups,
            live: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// Set what this slot means, everywhere it means anything.
    ///
    /// One update per metric rather than one per label. Setting four labels
    /// with four calls left a window where a reader could see a slot half-way
    /// through changing hands — a new task's pid beside the old task's comm.
    pub fn set(&self, slot: usize, mut labels: BTreeMap<String, String>) {
        // A live slot re-announcing the same labels is the same occupant:
        // keep its uid and write nothing. Every metric already holds these
        // labels.
        //
        // One generation and one uid for the whole assignment otherwise,
        // however many groups the slot spans: a cgroup id written by
        // `scheduler_runqueue` reaches three groups, and they must agree on
        // which occupant this is.
        let uid = {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            match live.get(&slot) {
                Some((current, _)) if *current == labels => return,
                _ => {
                    let uid = mint_uid(next_generation());
                    live.insert(slot, (labels.clone(), uid.clone()));
                    uid
                }
            }
        };
        labels.insert(UID_LABEL.to_string(), uid);
        for (_, metrics) in self.groups {
            for metric in *metrics {
                metric.set_metadata(slot, labels.clone());
            }
        }
    }

    /// The slot no longer means anything. Cleared so a later row is not
    /// attributed to a task that has exited.
    pub fn clear(&self, slot: usize) {
        // Forget the occupant, so the next assignment to this slot is a new
        // one whatever labels it brings: that is the PID-reuse case.
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&slot);
        for (_, metrics) in self.groups {
            for metric in *metrics {
                metric.clear_metadata(slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::metrics::GroupMetadata;
    use crate::agent::timing::AcquisitionGroup;

    /// A group metric that records what was written to it.
    ///
    /// This is why `GroupMetadata` is not gated to Linux: gating it would
    /// mean exercising `SlotIdentity` only where BPF runs.
    struct FakeGroup {
        // `BTreeMap` rather than `HashMap` so the static is const-constructible.
        written: std::sync::Mutex<BTreeMap<usize, Option<BTreeMap<String, String>>>>,
        /// How many times `set_metadata` was called, per slot.
        sets: std::sync::Mutex<BTreeMap<usize, usize>>,
    }

    impl FakeGroup {
        const fn new() -> Self {
            Self {
                written: std::sync::Mutex::new(BTreeMap::new()),
                sets: std::sync::Mutex::new(BTreeMap::new()),
            }
        }

        fn labels(&self, slot: usize) -> Option<BTreeMap<String, String>> {
            self.written.lock().unwrap().get(&slot).cloned().flatten()
        }

        fn sets(&self, slot: usize) -> usize {
            self.sets.lock().unwrap().get(&slot).copied().unwrap_or(0)
        }
    }

    impl GroupMetadata for FakeGroup {
        fn set_metadata(&self, idx: usize, labels: BTreeMap<String, String>) {
            self.written.lock().unwrap().insert(idx, Some(labels));
            *self.sets.lock().unwrap().entry(idx).or_default() += 1;
        }
        fn clear_metadata(&self, idx: usize) {
            self.written.lock().unwrap().insert(idx, None);
        }
    }

    static FAKE_A: FakeGroup = FakeGroup::new();
    static FAKE_B: FakeGroup = FakeGroup::new();
    static FAKE_METRICS: &[&dyn GroupMetadata] = &[&FAKE_A, &FAKE_B];
    static FAKE_ACQ: AcquisitionGroup = AcquisitionGroup::new("unattributed", "identity_probe");
    static FAKE_GROUPS: &[GroupMetrics] = &[(&FAKE_ACQ, FAKE_METRICS)];
    static FAKE_IDENTITY: SlotIdentity = SlotIdentity::new(FAKE_GROUPS);

    fn labels(comm: &str) -> BTreeMap<String, String> {
        [("comm".to_string(), comm.to_string())]
            .into_iter()
            .collect()
    }

    /// The labels a person sees: everything but the uid. Tests compare on
    /// these where the uid's value is not the point.
    fn visible(labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        labels
            .iter()
            .filter(|(k, _)| k.as_str() != UID_LABEL)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Setting a slot writes every metric of every group it spans, with one
    /// uid for the whole assignment. The tests share process-wide statics and
    /// run in parallel, so each owns a distinct slot.
    #[test]
    fn setting_a_slot_writes_every_metric() {
        FAKE_IDENTITY.set(3, labels("redis"));
        let a = FAKE_A.labels(3).expect("written");
        let b = FAKE_B.labels(3).expect("written");
        assert_eq!(visible(&a), labels("redis"));
        assert_eq!(a, b, "every metric carries the same labels and uid");
    }

    /// A live slot setting the same labels again is the same occupant
    /// saying its name again — `drivehealth` does it for every drive on
    /// every sweep, `ethtool` for every interface on every refresh. It keeps
    /// its uid, or every such series would split at every refresh, and it
    /// writes nothing, since nothing a metric holds has changed.
    #[test]
    fn a_re_announcement_keeps_its_uid_and_writes_nothing() {
        FAKE_IDENTITY.set(13, labels("nvme0"));
        let first = FAKE_A.labels(13).unwrap();
        FAKE_IDENTITY.set(13, labels("nvme0"));
        FAKE_IDENTITY.set(13, labels("nvme0"));
        let again = FAKE_A.labels(13).unwrap();
        assert_eq!(
            again[UID_LABEL], first[UID_LABEL],
            "same occupant, same uid"
        );
        assert_eq!(FAKE_A.sets(13), 1, "one assignment was written, not three");

        // A different name on a live slot is a new occupant even without a
        // clear between: an exec changed the comm.
        FAKE_IDENTITY.set(13, labels("nvme0n1"));
        let renamed = FAKE_A.labels(13).unwrap();
        assert_ne!(renamed[UID_LABEL], first[UID_LABEL]);
        assert_eq!(FAKE_A.sets(13), 2);
    }

    /// The case the uid exists for. A slot assigned twice with identical
    /// labels — a PID wrapped onto the same comm, a cgroup recreated at the
    /// same path — is two occupants, and a reader keying on labels alone would
    /// fuse them into one series with a counter reset in the middle. Each
    /// assignment mints its own uid, and the uid rides in the labels so every
    /// consumer of the labels gets it without being taught about it.
    #[test]
    fn a_reassignment_with_identical_labels_is_a_different_occupant() {
        FAKE_IDENTITY.set(11, labels("valkey"));
        let first = FAKE_A.labels(11).unwrap();
        // The task exits and the kernel hands its pid to a new task with
        // the same comm.
        FAKE_IDENTITY.clear(11);
        assert_eq!(FAKE_A.labels(11), None, "the clear reached the metric");
        FAKE_IDENTITY.set(11, labels("valkey"));
        let second = FAKE_A.labels(11).unwrap();

        assert_eq!(visible(&first), visible(&second));
        assert_ne!(
            first[UID_LABEL], second[UID_LABEL],
            "same labels, different occupant"
        );
        assert!(
            metriken_query::is_internal_label(UID_LABEL),
            "the uid is hidden by the same rule that hides __name__"
        );
        assert_eq!(
            second[UID_LABEL].len(),
            16,
            "sixteen hex characters: {}",
            second[UID_LABEL]
        );
    }

    /// A slot id spanning several groups is written to each, with one uid.
    ///
    /// One cgroup id written by `scheduler_runqueue` reaches three groups,
    /// and each group's schema carries its own copy of the labels.
    #[test]
    fn a_slot_spanning_several_groups_is_written_to_each() {
        static ACQ_ONE: AcquisitionGroup = AcquisitionGroup::new("unattributed", "span_one");
        static ACQ_TWO: AcquisitionGroup = AcquisitionGroup::new("unattributed", "span_two");
        static SPAN_A: FakeGroup = FakeGroup::new();
        static SPAN_B: FakeGroup = FakeGroup::new();
        static SPAN_GROUPS: &[GroupMetrics] = &[(&ACQ_ONE, &[&SPAN_A]), (&ACQ_TWO, &[&SPAN_B])];
        static SPANNING: SlotIdentity = SlotIdentity::new(SPAN_GROUPS);

        SPANNING.set(21, labels("shared"));
        let a = SPAN_A.labels(21).unwrap();
        let b = SPAN_B.labels(21).unwrap();
        assert_eq!(visible(&a), labels("shared"));
        assert_eq!(
            a[UID_LABEL], b[UID_LABEL],
            "one assignment, one uid across every group it spans"
        );
    }
}
