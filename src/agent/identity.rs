//! What a slot means, published when it changes.
//!
//! A group's values are positional, and what slot `v3` *is* — which task,
//! which cgroup — changes while the agent runs. A subscriber needs to be told,
//! and the question is where that telling comes from.
//!
//! # Why this is a broadcast and not a diff
//!
//! The obvious place is the sampling pass: walk the group, compare what each
//! slot means now against what it meant last tick, and emit the differences.
//! That is what the first version did, and it is wrong in a way no amount of
//! optimisation fixes. The change happens at an exact known moment — a
//! `task_info` event arriving, a `task_exit` clearing a slot — and
//! reconstructing it later by comparing the whole world costs O(live slots)
//! every tick whether anything moved or not. Measured against v5.20.0 on a
//! 32-CPU host, that reconstruction cost **1.24x the agent's entire sampling
//! CPU**, and it was paid by every consumer, including the ones scraping
//! `/metrics/binary` that have no use for identity at all.
//!
//! Publishing at the write is O(1) per actual change. A tick that moves
//! nothing costs nothing, because nothing is asked.
//!
//! # Current state is not kept here
//!
//! A subscriber connecting needs the whole picture, not just what changed
//! since. That picture already exists: metriken holds each group's live
//! metadata, and reading it is what the snapshot builder already does. So this
//! module keeps no copy — a new subscriber reads the current state once, at
//! connect, and follows the broadcast from there. One authority, no second
//! copy to drift.
//!
//! # The generation
//!
//! Every published change bumps a counter. A subscriber that has applied
//! everything up to generation N can be handed rows stamped N and know it is
//! current; one that fell behind sees a gap. It is monotonic and per process,
//! which is all the ordering a single agent needs.
//!
//! Connecting without a race means subscribing FIRST, then reading the current
//! state, then discarding any buffered change at or below the generation that
//! read saw. Taking the snapshot first would lose every change that landed
//! between the snapshot and the subscribe.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;

/// How many changes a slow subscriber may fall behind before it is told to
/// resync rather than being caught up.
///
/// A bounded channel is not a tuning choice here. The publisher is a BPF
/// ringbuf handler on the sampling path, and it must never block: a subscriber
/// that stopped reading cannot be allowed to stall task accounting. Past this
/// bound the subscriber is dropped from the stream and reconnects with a fresh
/// read of the current state, which is cheaper than holding history for a
/// consumer that is not consuming.
const CHANNEL_DEPTH: usize = 8192;

/// One slot's identity changing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotChanged {
    /// The acquisition group this slot belongs to, as its two halves. Carried
    /// unformatted because a publisher is a BPF ringbuf handler: `"a/b"` would
    /// be an allocation on the path a task creation takes, for a string only a
    /// subscriber needs.
    pub sampler: &'static str,
    pub group: &'static str,
    pub slot: u32,
    /// What the slot now means, or `None` where it was cleared.
    pub labels: Option<BTreeMap<String, String>>,
    /// The publisher's counter after this change. A subscriber compares it
    /// against what it has applied.
    pub generation: u64,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// How many consumers currently want identity changes.
///
/// Zero is the normal state: an exporter scraping `/metrics/binary`, a recorder
/// polling `/metrics/rows`, anything that reads values and not identity. None
/// of them should pay for an index, and with nothing wanting one there is
/// nothing to publish and nothing to fold in — the same principle #1240
/// established for sampling itself.
static WANTED: AtomicU64 = AtomicU64::new(0);

/// Held for the life of a consumer that wants identity changes.
///
/// A guard rather than a pair of calls: a subscription that ended without
/// decrementing would leave the agent maintaining an index for a consumer that
/// has gone, forever.
pub struct Demand;

impl Demand {
    pub fn register() -> Self {
        WANTED.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for Demand {
    fn drop(&mut self) {
        WANTED.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether anything wants identity changes right now.
pub fn wanted() -> bool {
    WANTED.load(Ordering::Acquire) > 0
}

/// The process-wide channel. Created on first use rather than at startup so an
/// agent nobody subscribes to still allocates it once and never sends.
fn channel() -> &'static broadcast::Sender<SlotChanged> {
    static CHANNEL: std::sync::OnceLock<broadcast::Sender<SlotChanged>> =
        std::sync::OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(CHANNEL_DEPTH).0)
}

/// The generation as of now. Read alongside a state snapshot so a new
/// subscriber knows which buffered changes it has already accounted for.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Follow identity changes from now on.
///
/// Subscribe BEFORE reading current state, or a change landing between the two
/// is lost with nothing to notice it.
pub fn subscribe() -> broadcast::Receiver<SlotChanged> {
    channel().subscribe()
}

/// The label that names one occupant of a slot: `__uid__`.
///
/// A slot's labels say what it means — `comm=redis pid=4112` — and they can
/// be the same for two different things: a PID wraps, a cgroup is deleted and
/// recreated at the same path, a task restarts under the same name. A reader
/// keying series on labels alone would fuse the two into one series with a
/// reset in the middle, attributing one task's counter to another. The uid is
/// what tells them apart. It is minted here, at the assignment, once per
/// [`SlotIdentity::set`], and travels with the labels everywhere they go: the
/// snapshot's descriptors, a `.rez` column's metadata, the identity index a
/// stream carries. Two recorders watching one agent therefore see the same
/// uid for the same occupant without coordinating.
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

/// Take the next generation. Every assignment takes one, whether or not a
/// subscriber exists, because the uid minted from it is written into the
/// slot's labels either way.
fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

/// Publish one slot's new identity, assigned at `generation`.
///
/// Never blocks and never fails: with no subscribers the send is dropped,
/// which is the common case and the whole point — an agent nobody is
/// streaming from pays nothing here.
pub(crate) fn publish(
    acq: &'static crate::agent::timing::AcquisitionGroup,
    slot: u32,
    labels: Option<BTreeMap<String, String>>,
    generation: u64,
) {
    // Nothing wants these, so nothing is built. The channel has a receiver only
    // while a consumer holds a `Demand`, and a `send` into a live channel
    // clones the change into the ring — which on a host creating and exiting
    // hundreds of tasks a second is real work for nobody.
    if !wanted() {
        return;
    }
    // `send` errs only when there are no receivers, which is not an error.
    let _ = channel().send(SlotChanged {
        sampler: acq.sampler,
        group: acq.name,
        slot,
        labels,
        generation,
    });
}

/// One acquisition group and the metrics that carry its slots.
pub type GroupMetrics = (
    &'static crate::agent::timing::AcquisitionGroup,
    &'static [&'static dyn crate::agent::metrics::GroupMetadata],
);

/// Every `SlotIdentity` in the build, so current state can be read without
/// each sampler being asked.
///
/// A `linkme` slice for the same reason `ACQUISITION_GROUPS` is one: a sampler
/// that declared identity but forgot to register it would be invisible to a
/// connecting subscriber, and nothing would say so.
#[linkme::distributed_slice]
pub static SLOT_IDENTITIES: [&'static SlotIdentity];

/// What every group currently means, for a consumer that has just arrived.
///
/// The broadcast carries only what changes after a subscriber joins. A slot
/// assigned before it arrived and never touched again would otherwise never be
/// described — so a subscriber reads this once, then follows the stream.
pub fn current_state() -> Vec<(&'static str, &'static str, u32, BTreeMap<String, String>)> {
    let mut out = Vec::new();
    for identity in SLOT_IDENTITIES {
        for (acq, metrics) in identity.groups {
            // Every metric of a group agrees on what a slot means (#1248), so
            // the first that carries the slot answers for all of them.
            let Some(first) = metrics.first() else {
                continue;
            };
            for (slot, labels) in first.metadata_snapshot() {
                out.push((
                    acq.sampler,
                    acq.name,
                    slot as u32,
                    labels.into_iter().collect(),
                ));
            }
        }
    }
    out
}

/// The one way to write a slot's identity.
///
/// Holds the groups a slot id spans, each paired with the metrics that carry
/// it. A pair rather than two arguments because passing them separately let a
/// caller hand one group's acquisition to another group's metrics — it
/// compiled, and it would publish identity under the wrong stream's name.
/// Silently wrong attribution is the failure the whole index exists to
/// prevent.
///
/// SEVERAL groups, because one id routinely spans them: a cgroup id written by
/// `scheduler_runqueue` reaches `..._cgroup_context_switch`,
/// `..._cgroup_offcpu` and `..._cgroup_wait`, which are three streams. A
/// subscriber keeps identity per stream, so each needs its own entry — one
/// change published for three groups would leave two of them never told.
///
/// It is the only holder of [`GroupMetadata`], so nothing can write identity
/// through that trait without publishing it. That is narrower than it sounds:
/// metriken's group types keep their own inherent `insert_metadata`, so a
/// sampler holding a concrete `&CounterGroup` can still write unpublished
/// metadata and the compiler will not stop it. Closing that would mean newtype
/// wrappers around metriken's types; until then this is a convention the trait
/// enforces at its own call sites only.
///
/// The pairing itself is NOT machine-checked. A `&dyn GroupMetadata` gives no
/// way back to the metric's declared `acq_group`, so nothing verifies that the
/// metrics listed beside a group are the ones that actually belong to it. A
/// wrong pairing publishes identity under the wrong stream name, silently.
pub struct SlotIdentity {
    groups: &'static [GroupMetrics],
}

impl SlotIdentity {
    pub const fn new(groups: &'static [GroupMetrics]) -> Self {
        Self { groups }
    }

    /// Set what this slot means, everywhere it means anything, and tell
    /// subscribers.
    ///
    /// One update per metric rather than one per label. Setting four labels
    /// with four calls left a window where a reader could see a slot half-way
    /// through changing hands — a new task's pid beside the old task's comm —
    /// and gave the publish four changes to describe instead of one.
    pub fn set(&self, slot: usize, mut labels: BTreeMap<String, String>) {
        // One generation and one uid for the whole assignment, however many
        // groups the slot spans: a cgroup id written by `scheduler_runqueue`
        // reaches three streams, and they must agree on which occupant this
        // is.
        let generation = next_generation();
        labels.insert(UID_LABEL.to_string(), mint_uid(generation));
        for (acq, metrics) in self.groups {
            for metric in *metrics {
                metric.set_metadata(slot, labels.clone());
            }
            publish(acq, slot as u32, Some(labels.clone()), generation);
        }
    }

    /// The slot no longer means anything. Published for the same reason: a
    /// subscriber that is not told keeps attributing rows to a dead task.
    pub fn clear(&self, slot: usize) {
        let generation = next_generation();
        for (acq, metrics) in self.groups {
            for metric in *metrics {
                metric.clear_metadata(slot);
            }
            publish(acq, slot as u32, None, generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::metrics::GroupMetadata;
    use crate::agent::timing::AcquisitionGroup;

    /// A group metric that records what was written to it, so a test can check
    /// that `SlotIdentity` wrote AND published rather than one or the other.
    ///
    /// This is why `GroupMetadata` is not gated to Linux: the publish path is
    /// the thing most worth testing, and gating it would mean exercising it
    /// only where BPF runs.
    struct FakeGroup {
        // `BTreeMap` rather than `HashMap` so the static is const-constructible.
        written: std::sync::Mutex<BTreeMap<usize, Option<BTreeMap<String, String>>>>,
    }

    impl GroupMetadata for FakeGroup {
        fn set_metadata(&self, idx: usize, labels: BTreeMap<String, String>) {
            self.written.lock().unwrap().insert(idx, Some(labels));
        }
        fn clear_metadata(&self, idx: usize) {
            self.written.lock().unwrap().insert(idx, None);
        }
        fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)> {
            self.written
                .lock()
                .unwrap()
                .iter()
                .filter_map(|(slot, labels)| {
                    labels
                        .as_ref()
                        .map(|l| (*slot, l.clone().into_iter().collect()))
                })
                .collect()
        }
    }

    static FAKE_A: FakeGroup = FakeGroup {
        written: std::sync::Mutex::new(BTreeMap::new()),
    };
    static FAKE_B: FakeGroup = FakeGroup {
        written: std::sync::Mutex::new(BTreeMap::new()),
    };
    static FAKE_METRICS: &[&dyn GroupMetadata] = &[&FAKE_A, &FAKE_B];
    static FAKE_ACQ: AcquisitionGroup = AcquisitionGroup::new("unattributed", "identity_probe");
    static FAKE_GROUPS: &[GroupMetrics] = &[(&FAKE_ACQ, FAKE_METRICS)];
    static FAKE_IDENTITY: SlotIdentity = SlotIdentity::new(FAKE_GROUPS);

    fn labels(comm: &str) -> BTreeMap<String, String> {
        [("comm".to_string(), comm.to_string())]
            .into_iter()
            .collect()
    }

    /// Register demand for the duration of a test.
    ///
    /// Publishing is gated on somebody wanting identity, so a test that does
    /// not say so observes nothing — and one that relied on a SIBLING test's
    /// demand would pass or fail on scheduling.
    fn want() -> Demand {
        Demand::register()
    }

    /// Everything published so far, in order.
    ///
    /// The channel is process-wide and these tests run in parallel, so a
    /// sibling's publish lands in this receiver too. Each test owns a distinct
    /// slot and filters to its own — isolation by construction, rather than by
    /// hoping the scheduler interleaves kindly.
    ///
    /// Draining ALL of it once and filtering afterwards, rather than filtering
    /// while draining: a filtering drain throws away the messages it does not
    /// match, so a second call for a different slot finds an empty channel.
    fn drain(rx: &mut broadcast::Receiver<SlotChanged>) -> Vec<SlotChanged> {
        let mut out = Vec::new();
        while let Ok(change) = rx.try_recv() {
            out.push(change);
        }
        out
    }

    fn for_slot(changes: &[SlotChanged], slot: u32) -> Vec<&SlotChanged> {
        changes.iter().filter(|c| c.slot == slot).collect()
    }

    /// The write and the publish are one act. A version that wrote metriken and
    /// forgot to publish would leave a subscriber attributing rows to whatever
    /// the slot used to mean, with nothing to notice — there is no per-tick
    /// diff any more.
    #[test]
    fn setting_a_slot_writes_every_metric_and_publishes_once() {
        let _want = want();
        let mut rx = subscribe();
        FAKE_IDENTITY.set(3, labels("redis"));

        for fake in [&FAKE_A, &FAKE_B] {
            let written = fake.written.lock().unwrap().get(&3).cloned().flatten();
            assert_eq!(
                written.as_ref().map(visible),
                Some(labels("redis")),
                "every metric of the group carries the new identity"
            );
        }

        let all = drain(&mut rx);
        let mine = for_slot(&all, 3);
        assert_eq!(
            mine.len(),
            1,
            "one change, not one per metric — a subscriber applies slots, not writes"
        );
        assert_eq!(mine[0].sampler, "unattributed");
        assert_eq!(mine[0].group, "identity_probe");
        assert_eq!(mine[0].labels.as_ref().map(visible), Some(labels("redis")));
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

    /// The case the uid exists for. A slot assigned twice with identical
    /// labels — a PID wrapped onto the same comm, a cgroup recreated at the
    /// same path — is two occupants, and a reader keying on labels alone would
    /// fuse them into one series with a counter reset in the middle. Each
    /// assignment mints its own uid, and the uid rides in the labels so every
    /// consumer of the labels gets it without being taught about it.
    #[test]
    fn a_reassignment_with_identical_labels_is_a_different_occupant() {
        let _want = want();
        let mut rx = subscribe();
        FAKE_IDENTITY.set(11, labels("valkey"));
        FAKE_IDENTITY.set(11, labels("valkey"));

        let all = drain(&mut rx);
        let mine = for_slot(&all, 11);
        assert_eq!(mine.len(), 2, "two assignments, two changes");
        let uid = |c: &SlotChanged| c.labels.as_ref().unwrap()[UID_LABEL].clone();
        assert_ne!(
            uid(mine[0]),
            uid(mine[1]),
            "same labels, different occupant"
        );
        assert!(
            metriken_query::is_internal_label(UID_LABEL),
            "the uid is hidden by the same rule that hides __name__"
        );
        assert_eq!(
            uid(mine[1]).len(),
            16,
            "sixteen hex characters: {}",
            uid(mine[1])
        );
        assert!(
            mine[1].generation > mine[0].generation,
            "and the generation says which came later"
        );

        // What a subscriber connecting later reads is the same uid the
        // broadcast carried, not a fresh one: the uid is stored with the
        // labels, so seeding from `metadata_snapshot` (what `current_state`
        // reads; the fixture is not in the registered slice) and following
        // the stream agree on the occupant.
        let now = FAKE_A
            .metadata_snapshot()
            .into_iter()
            .find(|(slot, _)| *slot == 11)
            .map(|(_, labels)| labels[UID_LABEL].clone());
        assert_eq!(now.as_deref(), Some(uid(mine[1]).as_str()));
    }

    /// A slot id spanning several groups is published once PER GROUP.
    ///
    /// One cgroup id written by `scheduler_runqueue` reaches three streams. A
    /// subscriber keeps identity per stream, so one change covering three
    /// groups would leave two of them never told, and their rows would attribute
    /// against identity that never arrived.
    #[test]
    fn a_slot_spanning_several_groups_is_published_for_each() {
        let _want = want();
        static ACQ_ONE: AcquisitionGroup = AcquisitionGroup::new("unattributed", "span_one");
        static ACQ_TWO: AcquisitionGroup = AcquisitionGroup::new("unattributed", "span_two");
        static SPAN_A: FakeGroup = FakeGroup {
            written: std::sync::Mutex::new(BTreeMap::new()),
        };
        static SPAN_B: FakeGroup = FakeGroup {
            written: std::sync::Mutex::new(BTreeMap::new()),
        };
        static SPAN_GROUPS: &[GroupMetrics] = &[(&ACQ_ONE, &[&SPAN_A]), (&ACQ_TWO, &[&SPAN_B])];
        static SPANNING: SlotIdentity = SlotIdentity::new(SPAN_GROUPS);

        let mut rx = subscribe();
        SPANNING.set(21, labels("shared"));

        let all = drain(&mut rx);
        let mine = for_slot(&all, 21);
        assert_eq!(mine.len(), 2, "one change per group, not one per slot");
        let mut named: Vec<&str> = mine.iter().map(|c| c.group).collect();
        named.sort_unstable();
        assert_eq!(named, vec!["span_one", "span_two"]);
        assert!(
            mine.iter()
                .all(|c| c.labels.as_ref().map(visible) == Some(labels("shared"))),
            "and each names the same identity"
        );
        let uids: std::collections::BTreeSet<&str> = mine
            .iter()
            .map(|c| c.labels.as_ref().unwrap()[UID_LABEL].as_str())
            .collect();
        assert_eq!(
            uids.len(),
            1,
            "one assignment, one uid across every group it spans"
        );
    }

    /// A cleared slot is published too. A subscriber never told keeps a dead
    /// task's identity and attributes later rows to it.
    #[test]
    fn clearing_a_slot_publishes_its_absence() {
        let _want = want();
        let mut rx = subscribe();
        FAKE_IDENTITY.set(9, labels("gone"));
        FAKE_IDENTITY.clear(9);

        let all = drain(&mut rx);
        let mine = for_slot(&all, 9);
        assert_eq!(mine.len(), 2, "the set and the clear");
        assert_eq!(mine[1].labels, None, "absence, not an empty label set");
        assert_eq!(
            FAKE_A.written.lock().unwrap().get(&9),
            Some(&None),
            "and the metric was actually cleared"
        );
    }

    /// The generation only moves forward, so a subscriber can tell "I have
    /// everything up to N" from "I missed something".
    #[test]
    fn the_generation_advances_with_every_change() {
        let _want = want();
        let mut rx = subscribe();
        let before = generation();
        FAKE_IDENTITY.set(1, labels("a"));
        FAKE_IDENTITY.set(2, labels("b"));

        let all = drain(&mut rx);
        let first = for_slot(&all, 1);
        let second = for_slot(&all, 2);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(
            first[0].generation > before,
            "a change advances the generation"
        );
        assert!(
            second[0].generation > first[0].generation,
            "and the next advances it again"
        );
        assert!(generation() >= second[0].generation);
    }

    /// Publishing with nobody listening is not an error. An agent nobody
    /// streams from is the common case, and it pays a counter bump.
    #[test]
    fn publishing_with_no_subscribers_is_fine() {
        let _want = want();
        FAKE_IDENTITY.set(42, labels("alone"));
        let written = FAKE_A.written.lock().unwrap().get(&42).cloned().flatten();
        assert_eq!(
            written.as_ref().map(visible),
            Some(labels("alone")),
            "the write still happened"
        );
    }

    /// The ordering a connecting subscriber depends on: subscribe first, then
    /// read current state. A change landing between the two is delivered
    /// rather than lost, and the generation says whether the snapshot already
    /// covered it.
    #[test]
    fn a_change_after_subscribing_is_delivered() {
        let _want = want();
        let mut rx = subscribe();
        let at_snapshot = generation();
        FAKE_IDENTITY.set(7, labels("after"));

        let all = drain(&mut rx);
        let mine = for_slot(&all, 7);
        assert_eq!(mine.len(), 1, "delivered");
        assert!(
            mine[0].generation > at_snapshot,
            "so a subscriber knows this one is not already in the state it read"
        );
    }
}

#[cfg(test)]
mod registration_tests {
    /// Every sampler's `SlotIdentity` is in `SLOT_IDENTITIES`.
    ///
    /// A source scan rather than a runtime check, because the failure is an
    /// absence: an unregistered identity is a static nothing can enumerate. It
    /// publishes changes normally, so every test passes and every slot it
    /// assigned before a subscriber connected is never described to it — a
    /// subscriber attributing rows against identity it was never given is the
    /// exact failure this index exists to prevent.
    #[test]
    fn every_sampler_identity_is_registered() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent/samplers");
        let mut missing = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        {
            let text = std::fs::read_to_string(entry.path()).expect("sampler source is readable");
            let declared = text.matches("SlotIdentity::new(").count();
            let registered = text
                .matches("distributed_slice(crate::agent::identity::SLOT_IDENTITIES)")
                .count();
            if declared != registered {
                missing.push(format!(
                    "{}: {declared} declared, {registered} registered",
                    entry.path().display()
                ));
            }
        }
        assert!(
            missing.is_empty(),
            "a `SlotIdentity` that is not in `SLOT_IDENTITIES` is invisible to a \
             connecting subscriber:\n{}",
            missing.join("\n")
        );
    }
}
