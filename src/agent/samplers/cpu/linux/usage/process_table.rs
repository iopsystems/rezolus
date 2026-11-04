//! The userspace side of `process_table` in mod.bpf.c: which process holds
//! each bucket, and the move of each entry's counts into `process_cpu_usage`.
//!
//! Every CPU running a thread of a process adds to that process's one
//! `process_cpu_usage` slot, and with tens of CPUs doing so the cache line
//! moves between them on every add. A process given an entry is added to in
//! its CPU's copy of the table instead. The `CGROUP_SHARDS` copies are
//! assigned from the L3 topology (`cgroup_shard_of_cpu`). Where sysfs
//! reports that topology, no copy is shared by two L3 caches unless the host
//! has more L3 caches than copies.
//!
//! This module is the only writer of an entry's start time and tgid. Each
//! refresh it:
//!
//! 1. moves every held or draining entry's counts into the holder's
//!    `process_cpu_usage` slot while the holder is live, and discards them
//!    otherwise; a draining bucket becomes free;
//! 2. frees the bucket of a holder that is gone, no longer labelled, or used
//!    less than an eighth of a CPU since the last refresh, by zeroing its
//!    start time; the bucket drains until the next refresh;
//! 3. gives a free bucket to a labelled process that used at least half a
//!    CPU since the last refresh.
//!
//! A draining bucket is not given out in the refresh that frees it, so a BPF
//! add that read the old start time before it was zeroed lands before the
//! counts are last moved. Refreshes are at least three quarters of the agent's
//! TTL apart.
//!
//! The move adds to `process_cpu_usage` before it subtracts from the entry,
//! and the export reads only `process_cpu_usage`, so the move never
//! decreases the exported value. A held process's CPU since the move is in
//! the table, and is exported after the next refresh; the sweep records the
//! move's time with `PROCESS_USAGE_ACQ.values_fixed_at`, so the group's
//! window begins no later than that.
//!
//! The move reads the holder's marker again after adding. If the slot's
//! group has exited since the first read, the exit's zero of the slot may
//! have come before the add, and the move takes the add back. Where the
//! exit's two stores are not ordered (arm64), an add can still be left in a
//! dead group's slot until a new group with that slot starts and zeroes it,
//! as a late add after the exit can (account_process in mod.bpf.c). If the
//! tgid is reused by a group that starts during the move and is still
//! running, the add can land in that group's slot, or the take-back can
//! subtract from it.

use crate::agent::CGROUP_SHARDS;

use std::sync::atomic::{AtomicU64, Ordering};

/// `PROCESS_TABLE_BITS` in mod.bpf.c.
const BITS: u32 = 8;
/// Buckets in each copy.
const SLOTS: usize = 1 << BITS;
/// Words per entry, one cache line.
const WIDTH: usize = 8;
const KEY: usize = 0;
const TGID: usize = 1;
const COUNT: usize = 2;

/// A process uses at least `1 / CLAIM` of a CPU to be given a bucket.
const CLAIM: u64 = 2;
/// A holder using less than `1 / RELEASE` of a CPU gives its bucket up.
const RELEASE: u64 = 8;

/// The bucket the BPF side looks `tgid` up in.
fn bucket(tgid: u32) -> usize {
    (tgid.wrapping_mul(0x9E37_79B1) >> (32 - BITS)) as usize
}

/// The index of `bucket`'s entry in copy `copy`.
fn entry(copy: usize, bucket: usize) -> usize {
    (copy * SLOTS + bucket) * WIDTH
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bucket {
    Free,
    /// The BPF side adds the process's deltas to this bucket.
    Held {
        tgid: u32,
        key: u64,
    },
    /// Freed at the last refresh; may still hold counts that were added
    /// before the BPF side saw the start time zeroed.
    Draining {
        tgid: u32,
        key: u64,
    },
}

/// A labelled process: its `process_cpu_usage` slot, tgid and start time.
#[derive(Clone, Copy)]
pub(super) struct Process {
    pub slot: usize,
    pub tgid: u32,
    pub start_time: u64,
}

/// A test's code run with a process's slot, `process_cpu_usage` and
/// `process_start_times`.
#[cfg(test)]
type BeforeAdd = fn(slot: usize, count: &AtomicU64, usage: &[AtomicU64], markers: &[AtomicU64]);

/// What the table holds, and each labelled process's value at the last
/// refresh.
pub(super) struct Table {
    buckets: Vec<Bucket>,
    /// By slot: the start time and `process_cpu_usage` value at the last
    /// refresh.
    last: std::collections::BTreeMap<usize, (u64, u64)>,
    last_ns: Option<u64>,
    /// Runs between the move's liveness read and its add.
    #[cfg(test)]
    before_add: Option<BeforeAdd>,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            buckets: vec![Bucket::Free; SLOTS],
            last: Default::default(),
            last_ns: None,
            #[cfg(test)]
            before_add: None,
        }
    }
}

impl Table {
    /// One refresh at `now` (ns): `table` is `process_table`, `usage` is
    /// `process_cpu_usage`, and `markers` is `process_start_times`.
    /// `processes` are the labelled processes. Returns whether any bucket
    /// was in use before or after it, so that some process's value in
    /// `process_cpu_usage` is the one this refresh moved.
    pub(super) fn refresh(
        &mut self,
        table: &[AtomicU64],
        usage: &[AtomicU64],
        markers: &[AtomicU64],
        processes: &[Process],
        now: u64,
    ) -> bool {
        let marker = |tgid: u32| {
            markers
                .get(super::pid_slot(tgid as usize))
                .map_or(0, |m| m.load(Ordering::SeqCst))
                & !super::METADATA_PENDING
        };
        let live = |tgid: u32, key: u64| marker(tgid) == key;
        let in_use = |buckets: &[Bucket]| buckets.iter().any(|b| *b != Bucket::Free);
        let was_in_use = in_use(&self.buckets);

        for (b, bucket) in self.buckets.iter_mut().enumerate() {
            let (tgid, key) = match *bucket {
                Bucket::Free => continue,
                Bucket::Held { tgid, key } | Bucket::Draining { tgid, key } => (tgid, key),
            };
            let slot = super::pid_slot(tgid as usize);
            let keep = live(tgid, key);
            for copy in 0..CGROUP_SHARDS {
                let count = &table[entry(copy, b) + COUNT];
                let value = count.load(Ordering::Relaxed);
                if value == 0 {
                    continue;
                }
                if let Some(total) = usage.get(slot).filter(|_| keep) {
                    #[cfg(test)]
                    if let Some(hook) = self.before_add {
                        hook(slot, count, usage, markers);
                    }
                    total.fetch_add(value, Ordering::SeqCst);
                    // A group's last exit marks it exited and then zeroes the
                    // slot. If the slot's group has exited since `live`, the
                    // zero may have come before the add, so take the add back.
                    if marker(tgid) & super::INSTANCE_EXITED != 0 {
                        let mut v = total.load(Ordering::SeqCst);
                        while let Err(cur) = total.compare_exchange_weak(
                            v,
                            v.saturating_sub(value),
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            v = cur;
                        }
                    }
                }
                count.fetch_sub(value, Ordering::Relaxed);
            }
            if matches!(bucket, Bucket::Draining { .. }) {
                *bucket = Bucket::Free;
            }
        }

        let elapsed = self.last_ns.map(|last| now.saturating_sub(last));
        self.last_ns = Some(now);

        let mut last = std::collections::BTreeMap::new();
        let mut labelled = std::collections::BTreeSet::new();
        for p in processes {
            let value = usage.get(p.slot).map_or(0, |v| v.load(Ordering::Relaxed));
            let delta = match self.last.get(&p.slot) {
                Some(&(start_time, before)) if start_time == p.start_time => {
                    Some(value.saturating_sub(before))
                }
                _ => None,
            };
            last.insert(p.slot, (p.start_time, value));
            labelled.insert((p.tgid, p.start_time));

            let (Some(elapsed), Some(delta)) = (elapsed, delta) else {
                continue;
            };
            let b = bucket(p.tgid);
            let current = self.buckets[b];
            match current {
                Bucket::Held { tgid, key }
                    if (tgid, key) == (p.tgid, p.start_time)
                        && delta.saturating_mul(RELEASE) < elapsed =>
                {
                    self.release(table, b);
                }
                Bucket::Free
                    if delta.saturating_mul(CLAIM) >= elapsed && live(p.tgid, p.start_time) =>
                {
                    self.claim(table, b, p.tgid, p.start_time);
                }
                _ => {}
            }
        }
        self.last = last;

        let gone: Vec<usize> = self
            .buckets
            .iter()
            .enumerate()
            .filter_map(|(b, bucket)| match *bucket {
                Bucket::Held { tgid, key }
                    if !live(tgid, key) || !labelled.contains(&(tgid, key)) =>
                {
                    Some(b)
                }
                _ => None,
            })
            .collect();
        for b in gone {
            self.release(table, b);
        }
        was_in_use || in_use(&self.buckets)
    }

    /// Give bucket `b` to the process: the tgid first, then the start time,
    /// in every copy. A BPF add that reads the new start time with the
    /// previous holder's tgid goes to `process_cpu_usage`.
    fn claim(&mut self, table: &[AtomicU64], b: usize, tgid: u32, key: u64) {
        for copy in 0..CGROUP_SHARDS {
            table[entry(copy, b) + TGID].store(tgid as u64, Ordering::Relaxed);
        }
        for copy in 0..CGROUP_SHARDS {
            table[entry(copy, b) + KEY].store(key, Ordering::Release);
        }
        self.buckets[b] = Bucket::Held { tgid, key };
    }

    /// Stop the BPF side adding to bucket `b`; its counts are moved at the
    /// next refresh.
    fn release(&mut self, table: &[AtomicU64], b: usize) {
        let Bucket::Held { tgid, key } = self.buckets[b] else {
            return;
        };
        for copy in 0..CGROUP_SHARDS {
            table[entry(copy, b) + KEY].store(0, Ordering::Relaxed);
        }
        self.buckets[b] = Bucket::Draining { tgid, key };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: u64 = 1_000_000_000;
    /// The table's length in words.
    const ENTRIES: usize = CGROUP_SHARDS * SLOTS * WIDTH;
    /// Slots in the test maps: `pid_slot` keeps a tgid below 2^16 below it.
    const PIDS: usize = 1 << 16;

    struct Maps {
        table: Vec<AtomicU64>,
        usage: Vec<AtomicU64>,
        markers: Vec<AtomicU64>,
    }

    impl Maps {
        fn new() -> Self {
            let words = |n| (0..n).map(|_| AtomicU64::new(0)).collect();
            Self {
                table: words(ENTRIES),
                usage: words(PIDS),
                markers: words(PIDS),
            }
        }

        fn start(&self, tgid: u32, start_time: u64) -> Process {
            let slot = super::super::pid_slot(tgid as usize);
            self.markers[slot].store(start_time, Ordering::Relaxed);
            self.usage[slot].store(0, Ordering::Relaxed);
            Process {
                slot,
                tgid,
                start_time,
            }
        }

        fn exit(&self, p: &Process) {
            self.markers[p.slot].store(
                p.start_time | super::super::INSTANCE_EXITED,
                Ordering::Relaxed,
            );
            self.usage[p.slot].store(0, Ordering::Relaxed);
        }

        /// What the BPF side does with a delta: the entry in `copy` when it
        /// holds the process, else the process's slot.
        fn add(&self, p: &Process, copy: usize, delta: u64) {
            let e = entry(copy, bucket(p.tgid));
            if self.table[e + KEY].load(Ordering::Relaxed) == p.start_time
                && self.table[e + TGID].load(Ordering::Relaxed) == p.tgid as u64
            {
                self.table[e + COUNT].fetch_add(delta, Ordering::Relaxed);
            } else {
                self.usage[p.slot].fetch_add(delta, Ordering::Relaxed);
            }
        }

        fn in_table(&self, p: &Process) -> u64 {
            (0..CGROUP_SHARDS)
                .map(|copy| {
                    let e = entry(copy, bucket(p.tgid));
                    let held = self.table[e + KEY].load(Ordering::Relaxed) == p.start_time
                        && self.table[e + TGID].load(Ordering::Relaxed) == p.tgid as u64;
                    if held {
                        self.table[e + COUNT].load(Ordering::Relaxed)
                    } else {
                        0
                    }
                })
                .sum()
        }

        fn exported(&self, p: &Process) -> u64 {
            self.usage[p.slot].load(Ordering::Relaxed)
        }

        fn refresh(&self, t: &mut Table, processes: &[Process], now: u64) -> bool {
            t.refresh(&self.table, &self.usage, &self.markers, processes, now)
        }
    }

    /// The BPF side and this module agree on the layout.
    #[test]
    fn bpf_source_has_the_same_layout() {
        let source = include_str!("mod.bpf.c");
        assert!(source.contains(&format!("#define PROCESS_TABLE_BITS {BITS}\n")));
        assert!(source.contains(&format!("#define PROCESS_ENTRY_WIDTH {WIDTH}\n")));
        assert!(source.contains(&format!("#define PROCESS_ENTRY_KEY {KEY}\n")));
        assert!(source.contains(&format!("#define PROCESS_ENTRY_TGID {TGID}\n")));
        assert!(source.contains(&format!("#define PROCESS_ENTRY_COUNT {COUNT}\n")));
        assert!(source.contains("u32 bucket = (tgid * 0x9E3779B1U) >> (32 - PROCESS_TABLE_BITS);"));
        assert!(source
            .contains("u32 idx = (copy * PROCESS_TABLE_SLOTS + bucket) * PROCESS_ENTRY_WIDTH;"));
        assert!(source.contains(
            "__uint(max_entries, CGROUP_SHARDS * PROCESS_TABLE_SLOTS * PROCESS_ENTRY_WIDTH);"
        ));
    }

    /// A busy process is given its bucket at the second refresh, its deltas
    /// then go to the table, and every refresh moves them into its slot with
    /// nothing lost.
    #[test]
    fn busy_process_is_given_a_bucket_and_counted_in_full() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(4242, 7 * SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, 2 * SEC);
        maps.refresh(&mut t, &[p], 2 * SEC);
        assert_eq!(
            t.buckets[bucket(p.tgid)],
            Bucket::Held {
                tgid: p.tgid,
                key: p.start_time
            }
        );

        let mut total = 2 * SEC;
        for (i, copy) in [0, 3, 15].into_iter().enumerate() {
            maps.add(&p, copy, SEC);
            total += SEC;
            assert_eq!(maps.in_table(&p), SEC, "the add went to the table");
            maps.refresh(&mut t, &[p], (3 + i as u64) * SEC);
            assert_eq!(maps.in_table(&p), 0);
            assert_eq!(maps.exported(&p), total);
        }
    }

    /// A process using less than half a CPU is not given a bucket.
    #[test]
    fn light_process_stays_in_its_slot() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC / 2 - 1);
        maps.refresh(&mut t, &[p], 2 * SEC);
        assert_eq!(t.buckets[bucket(p.tgid)], Bucket::Free);
    }

    /// A holder that goes idle drains for one refresh, keeping the counts
    /// added before the release, and the bucket is free after that.
    #[test]
    fn idle_holder_drains_then_frees() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        assert!(!maps.refresh(&mut t, &[p], SEC), "nothing held");
        maps.add(&p, 0, SEC);
        assert!(maps.refresh(&mut t, &[p], 2 * SEC), "claimed");
        maps.add(&p, 1, SEC / 16);
        assert!(maps.refresh(&mut t, &[p], 3 * SEC), "released");
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Draining { .. }));
        assert!(
            (0..CGROUP_SHARDS).all(|copy| maps.table[entry(copy, bucket(p.tgid)) + KEY]
                .load(Ordering::Relaxed)
                == 0)
        );

        // An add after the release goes to the slot.
        maps.add(&p, 1, 7);
        assert_eq!(maps.exported(&p), SEC + SEC / 16 + 7);

        // An add that read the start time before the release.
        let e = entry(2, bucket(p.tgid));
        maps.table[e + COUNT].fetch_add(5, Ordering::Relaxed);
        assert!(maps.refresh(&mut t, &[p], 4 * SEC), "drained");
        assert_eq!(t.buckets[bucket(p.tgid)], Bucket::Free);
        assert_eq!(maps.exported(&p), SEC + SEC / 16 + 7 + 5);
        assert_eq!(maps.table[e + COUNT].load(Ordering::Relaxed), 0);
        assert!(!maps.refresh(&mut t, &[p], 5 * SEC), "nothing held");
    }

    /// A holder that exits has its bucket freed, and its counts are not moved
    /// into the slot its exit zeroed.
    #[test]
    fn exited_holder_is_released_and_its_counts_discarded() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC);
        maps.refresh(&mut t, &[p], 2 * SEC);
        maps.add(&p, 0, SEC);
        maps.exit(&p);
        maps.refresh(&mut t, &[], 3 * SEC);
        assert_eq!(maps.exported(&p), 0);
        maps.refresh(&mut t, &[], 4 * SEC);
        assert_eq!(t.buckets[bucket(p.tgid)], Bucket::Free);
        assert!(
            (0..ENTRIES).all(|i| i % WIDTH != COUNT || maps.table[i].load(Ordering::Relaxed) == 0)
        );
    }

    /// A process whose last exit has marked it exited is not given a
    /// bucket, even before the exit zeroes its slot.
    #[test]
    fn process_marked_exited_is_not_given_a_bucket() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC);
        maps.markers[p.slot].store(
            p.start_time | super::super::INSTANCE_EXITED,
            Ordering::Relaxed,
        );
        maps.refresh(&mut t, &[p], 2 * SEC);
        assert_eq!(t.buckets[bucket(p.tgid)], Bucket::Free);
    }

    /// A holder marked exited is released while still labelled and busy,
    /// and the counts in its entry are not moved.
    #[test]
    fn holder_marked_exited_is_released_while_labelled() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC);
        maps.refresh(&mut t, &[p], 2 * SEC);
        maps.add(&p, 0, SEC);
        maps.markers[p.slot].store(
            p.start_time | super::super::INSTANCE_EXITED,
            Ordering::Relaxed,
        );
        maps.usage[p.slot].fetch_add(SEC, Ordering::Relaxed);
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Draining { .. }));
        assert_eq!(maps.exported(&p), 2 * SEC);
    }

    /// Marks the group in `slot` exited and zeroes the slot, as its last
    /// exit does.
    fn exit_in_slot(slot: usize, usage: &[AtomicU64], markers: &[AtomicU64]) {
        markers[slot].fetch_or(super::super::INSTANCE_EXITED, Ordering::SeqCst);
        usage[slot].store(0, Ordering::SeqCst);
    }

    /// A busy process holding its bucket, with `SEC` in the table.
    fn holder_with_counts(maps: &Maps, t: &mut Table) -> Process {
        let p = maps.start(100, SEC);
        maps.refresh(t, &[p], SEC);
        maps.add(&p, 0, SEC);
        maps.refresh(t, &[p], 2 * SEC);
        maps.add(&p, 0, SEC);
        p
    }

    /// An exit that marks the group and zeroes its slot after the move read
    /// the marker and before it adds: the move takes its add back.
    #[test]
    fn exit_during_the_move_is_taken_back() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        t.before_add = Some(|slot, _, usage, markers| exit_in_slot(slot, usage, markers));
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert_eq!(maps.exported(&p), 0);
    }

    /// As above, with adds to the slot after the exit: the move takes back
    /// what it added and leaves those.
    #[test]
    fn exit_during_the_move_takes_back_only_the_move() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        t.before_add = Some(|slot, _, usage, markers| {
            exit_in_slot(slot, usage, markers);
            usage[slot].fetch_add(3 * SEC, Ordering::SeqCst);
        });
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert_eq!(maps.exported(&p), 3 * SEC);
    }

    /// A BPF add to the entry while the move runs stays in the entry and is
    /// moved at the next refresh.
    #[test]
    fn add_during_the_move_is_kept() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        t.before_add = Some(|_, count, _, _| {
            count.fetch_add(5, Ordering::Relaxed);
        });
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert_eq!(maps.in_table(&p), 5);
        t.before_add = None;
        maps.refresh(&mut t, &[p], 4 * SEC);
        assert_eq!(maps.exported(&p), 2 * SEC + 5);
    }

    /// Exactly half a CPU over a refresh is enough for a bucket.
    #[test]
    fn half_a_cpu_is_given_a_bucket() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC / 2);
        maps.refresh(&mut t, &[p], 2 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Held { .. }));
    }

    /// A holder keeps its bucket at exactly an eighth of a CPU and gives it
    /// up below that.
    #[test]
    fn holder_keeps_its_bucket_down_to_an_eighth_of_a_cpu() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        maps.refresh(&mut t, &[p], 3 * SEC);
        maps.add(&p, 0, SEC / 8);
        maps.refresh(&mut t, &[p], 4 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Held { .. }));
        maps.add(&p, 0, SEC / 8 - 1);
        maps.refresh(&mut t, &[p], 5 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Draining { .. }));
    }

    /// A new group with the same tgid does not inherit the old group's
    /// bucket, its deltas go to its own slot, and the old group's counts are
    /// not moved into it.
    #[test]
    fn reused_tgid_does_not_match_the_old_entry() {
        let maps = Maps::new();
        let mut t = Table::default();
        let old = maps.start(100, SEC);

        maps.refresh(&mut t, &[old], SEC);
        maps.add(&old, 0, SEC);
        maps.refresh(&mut t, &[old], 2 * SEC);
        maps.add(&old, 0, SEC);
        maps.exit(&old);
        let new = maps.start(100, 9 * SEC);
        maps.add(&new, 0, 3);
        assert_eq!(maps.exported(&new), 3);
        maps.refresh(&mut t, &[new], 3 * SEC);
        assert_eq!(maps.exported(&new), 3);
    }

    /// A new group in a reused slot has no usage before its first refresh,
    /// so it is not given a bucket there.
    #[test]
    fn new_group_in_a_reused_slot_starts_from_its_first_refresh() {
        let maps = Maps::new();
        let mut t = Table::default();
        let old = maps.start(100, SEC);

        maps.refresh(&mut t, &[old], SEC);
        maps.exit(&old);
        let new = maps.start(100, 9 * SEC);
        maps.add(&new, 0, SEC);
        maps.refresh(&mut t, &[new], 2 * SEC);
        assert_eq!(t.buckets[bucket(new.tgid)], Bucket::Free);
    }

    /// The tgid is reused by a group that also exits during the move: the
    /// move's add does not stay in the dead slot.
    #[test]
    fn add_into_a_reused_dead_slot_is_taken_back() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        t.before_add = Some(|slot, _, usage, markers| {
            exit_in_slot(slot, usage, markers);
            markers[slot].store(9 * SEC, Ordering::SeqCst);
            usage[slot].store(0, Ordering::SeqCst);
            exit_in_slot(slot, usage, markers);
        });
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert_eq!(maps.exported(&p), 0);
    }

    /// A holder whose leader rename left its marker pending keeps its bucket
    /// and its counts.
    #[test]
    fn pending_holder_is_live() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = holder_with_counts(&maps, &mut t);

        maps.markers[p.slot].store(
            p.start_time | super::super::METADATA_PENDING,
            Ordering::Relaxed,
        );
        maps.refresh(&mut t, &[p], 3 * SEC);
        assert_eq!(maps.exported(&p), 2 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Held { .. }));
    }

    /// Two labelled processes whose tgids share a bucket.
    fn colliding(maps: &Maps) -> (Process, Process) {
        let first = 1u32;
        let second = (2..PIDS as u32)
            .find(|&tgid| bucket(tgid) == bucket(first))
            .expect("a colliding tgid");
        (maps.start(first, SEC), maps.start(second, 2 * SEC))
    }

    /// A bucket freed in a refresh is not given to a colliding busy process
    /// in that refresh.
    #[test]
    fn draining_bucket_is_not_claimed_in_the_same_refresh() {
        let maps = Maps::new();
        let mut t = Table::default();
        let (a, b) = colliding(&maps);

        maps.refresh(&mut t, &[a, b], SEC);
        maps.add(&a, 0, SEC);
        maps.refresh(&mut t, &[a, b], 2 * SEC);
        maps.add(&b, 0, SEC);
        maps.refresh(&mut t, &[a, b], 3 * SEC);
        assert!(matches!(
            t.buckets[bucket(a.tgid)],
            Bucket::Draining { tgid, .. } if tgid == a.tgid
        ));
    }

    /// An idle labelled process sharing a busy holder's bucket does not free
    /// it.
    #[test]
    fn idle_colliding_process_does_not_release_the_holder() {
        let maps = Maps::new();
        let mut t = Table::default();
        let (a, b) = colliding(&maps);

        maps.refresh(&mut t, &[a, b], SEC);
        maps.add(&a, 0, SEC);
        maps.refresh(&mut t, &[a, b], 2 * SEC);
        maps.add(&a, 0, SEC);
        maps.refresh(&mut t, &[a, b], 3 * SEC);
        assert!(matches!(
            t.buckets[bucket(a.tgid)],
            Bucket::Held { tgid, .. } if tgid == a.tgid
        ));
    }

    /// Two busy processes in one bucket: the first holds it and the second
    /// is counted in its slot.
    #[test]
    fn colliding_process_stays_in_its_slot() {
        let maps = Maps::new();
        let mut t = Table::default();
        let (a, b) = colliding(&maps);

        maps.refresh(&mut t, &[a, b], SEC);
        maps.add(&a, 0, SEC);
        maps.add(&b, 0, SEC);
        maps.refresh(&mut t, &[a, b], 2 * SEC);
        maps.add(&a, 0, SEC);
        maps.add(&b, 0, SEC);
        assert_eq!(maps.in_table(&a), SEC);
        assert_eq!(maps.in_table(&b), 0);
        maps.refresh(&mut t, &[a, b], 3 * SEC);
        assert_eq!(maps.exported(&a), 2 * SEC);
        assert_eq!(maps.exported(&b), 2 * SEC);
    }

    /// A process whose labels were released gives its bucket up even while
    /// it runs.
    #[test]
    fn unlabelled_holder_is_released() {
        let maps = Maps::new();
        let mut t = Table::default();
        let p = maps.start(100, SEC);

        maps.refresh(&mut t, &[p], SEC);
        maps.add(&p, 0, SEC);
        maps.refresh(&mut t, &[p], 2 * SEC);
        maps.refresh(&mut t, &[], 3 * SEC);
        assert!(matches!(t.buckets[bucket(p.tgid)], Bucket::Draining { .. }));
    }
}
