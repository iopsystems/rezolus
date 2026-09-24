//! A group table read through the identity index: its slot columns split by
//! occupant before the query engine sees them.
//!
//! A group table's columns are its slots — `{metric_id}x{slot}` — and a slot
//! is reused: a task exits and a new one lands in its BPF map slot, a cgroup
//! id is recycled, a device is remounted. What the slot MEANS at each moment
//! is in the identity index (`caller_rows`: one [`IndexEntry`] per stream per
//! change), not in the column. Read as parquet, the column's field metadata
//! carries whatever labels the slot had when the segment was built, and every
//! row of that column is filed under them, whoever occupied the slot at the
//! time. This module replays the index into per-slot occupancy spans
//! ([`Occupants`]) and hands them to the segmented reader as a
//! [`ColumnRelabel`]: at open it says what label sets each slot column can
//! present as, and at query time it cuts a column's samples into runs by
//! occupant. The reader stays lazy — segments fetched as a query touches
//! them, nothing decoded that the query does not read — and the split costs
//! a binary search per run boundary rather than a decode of the table.
//!
//! It used to decode every segment into a `MemoryStore` at first query. On a
//! ten-hour archive whose task table is 159 segments of up to 2,851 columns,
//! that is the whole table in memory, which is what the archive reader had
//! just stopped doing for every other table.
//!
//! The archive's rows do not change; only what they are attributed to. That
//! is why the split happens on read: the recorder writes what it received,
//! and the seam between two occupants of one slot is a fact of the index, not
//! of the values.
//!
//! # Where the labels come from
//!
//! A series' labels are the column's own (its storage keys removed, exactly
//! as the parquet loader does it) with the occupant's index labels laid over
//! the top. The index wins on a conflict. Today's archives carry the same
//! labels in both places — a column's metadata still holds the occupant's
//! labels and its `__uid__` — so the overlay changes nothing and the two read
//! paths agree series for series, which is what the oracle test in the reader
//! pins. Once the writer stops copying identity into column metadata (#1224
//! §2, step 7), the index is the only place the labels live, and this path is
//! the one that still knows them.
//!
//! A sample with no occupant on record keeps the column's own labels, which
//! the reader then does not find in its identity index and drops with a
//! warning; [`OccupantRelabel::unattributed`] counts them. In a well-formed
//! archive there are none: rows and the entries describing them commit
//! together, and retention cuts entries back only to a `Full` at or before
//! the row cutoff.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use metriken_query::{ColumnRelabel, Labels, Run};

use crate::index::{EntryKind, IndexEntry};

/// One slot's occupant over `[from, to)`, in row timestamps. `to` is `None`
/// while the occupant is still live at the end of the index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupancy {
    pub from: u64,
    pub to: Option<u64>,
    pub labels: BTreeMap<String, String>,
}

impl Occupancy {
    fn covers(&self, ts: u64) -> bool {
        ts >= self.from && self.to.is_none_or(|to| ts < to)
    }
}

/// Every slot's occupancy history for one stream, replayed from its index
/// entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Occupants {
    spans: BTreeMap<u32, Vec<Occupancy>>,
    /// `Delta` entries that arrived before the stream's first `Full` and were
    /// skipped: they describe a change to a set the reader never saw.
    pub skipped_before_full: usize,
}

impl Occupants {
    /// Replay `entries` — `(ts, blob)` in `(ts, seq)` order, as
    /// `RezDb::read_caller_rows` returns them — into occupancy spans.
    ///
    /// An entry stamped `ts` takes effect AT `ts`: the recorder commits a
    /// tick's rows and the entries describing them together, stamped alike,
    /// so the row at an entry's timestamp already carries the new occupant's
    /// values. A span therefore closes at the timestamp of the entry that
    /// ended it and the next one opens there.
    ///
    /// A `Delta` before the first `Full` is skipped and counted, not an
    /// error: the archive is still readable, and the rows it would have
    /// described fall back to their column's labels.
    pub fn replay(entries: &[(u64, Vec<u8>)]) -> Result<Self, String> {
        // Slot -> (since, labels) for every slot currently occupied. Kept
        // directly rather than behind `SlotIndex`: the reader needs the spans,
        // not the state hash, and the hash costs a serialization per slot per
        // change. Measured before this: 248k entries of a churning task
        // stream (414/s over ten minutes) took 20 s to replay, against 0.4 s
        // for the parquet path on the same rows, because every entry also
        // diffed the whole live set.
        let mut open: BTreeMap<u32, (u64, BTreeMap<String, String>)> = BTreeMap::new();
        let mut out = Occupants::default();
        let mut seen_full = false;

        for (ts, blob) in entries {
            let entry =
                IndexEntry::decode(blob).map_err(|e| format!("index entry at {ts}: {e}"))?;
            match entry.kind {
                EntryKind::Delta if !seen_full => {
                    out.skipped_before_full += 1;
                }
                EntryKind::Delta => {
                    // Only the slots the entry names can have changed, so
                    // only they are touched: the cost of a change is the size
                    // of the change, not of the live set.
                    for slot in &entry.removed {
                        if let Some((from, labels)) = open.remove(slot) {
                            out.close(*slot, from, Some(*ts), labels);
                        }
                    }
                    for e in entry.slots {
                        if open.get(&e.slot).is_some_and(|(_, l)| *l == e.labels) {
                            continue;
                        }
                        if let Some((from, labels)) = open.remove(&e.slot) {
                            out.close(e.slot, from, Some(*ts), labels);
                        }
                        open.insert(e.slot, (*ts, e.labels));
                    }
                }
                EntryKind::Full => {
                    // A `Full` states the set and names nothing that left it,
                    // so this is the one place the whole live set is diffed.
                    // Once per restatement, not per change.
                    seen_full = true;
                    let next: BTreeMap<u32, BTreeMap<String, String>> = entry
                        .slots
                        .into_iter()
                        .map(|e| (e.slot, e.labels))
                        .collect();
                    let ended: Vec<u32> = open
                        .iter()
                        .filter(|(slot, (_, labels))| next.get(slot) != Some(labels))
                        .map(|(slot, _)| *slot)
                        .collect();
                    for slot in ended {
                        let (from, labels) = open.remove(&slot).expect("listed from open");
                        out.close(slot, from, Some(*ts), labels);
                    }
                    for (slot, labels) in next {
                        open.entry(slot).or_insert((*ts, labels));
                    }
                }
            }
        }
        for (slot, (from, labels)) in open {
            out.close(slot, from, None, labels);
        }
        Ok(out)
    }

    fn close(&mut self, slot: u32, from: u64, to: Option<u64>, labels: BTreeMap<String, String>) {
        // Two entries at one timestamp (a `Full` then a `Delta`, say) can
        // open and close an occupant at the same instant. No row can fall in
        // an empty span, so it is not kept.
        if to == Some(from) {
            return;
        }
        self.spans
            .entry(slot)
            .or_default()
            .push(Occupancy { from, to, labels });
    }

    /// Who occupied `slot` at `ts`, if the index says.
    pub fn at(&self, slot: u32, ts: u64) -> Option<&Occupancy> {
        let spans = self.spans.get(&slot)?;
        // Spans are pushed in time order and never overlap, so the candidate
        // is the last one starting at or before `ts`.
        let i = spans.partition_point(|s| s.from <= ts);
        let span = spans[..i].last()?;
        span.covers(ts).then_some(span)
    }

    /// The slots the index ever named.
    pub fn slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.spans.keys().copied()
    }

    /// Every span of `slot`, oldest first.
    pub fn spans(&self, slot: u32) -> &[Occupancy] {
        self.spans.get(&slot).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The identity index of one stream, as the segmented reader's
/// [`ColumnRelabel`].
pub struct OccupantRelabel {
    occupants: Occupants,
    /// Label keys the index supplies. A filter on one of them cannot be
    /// asked of a column, whose own labels do not carry it.
    supplied: BTreeSet<String>,
    /// Samples of slot columns that had no occupant on record.
    unattributed: AtomicUsize,
}

impl OccupantRelabel {
    pub fn new(occupants: Occupants) -> Self {
        let supplied = occupants
            .spans
            .values()
            .flatten()
            .flat_map(|o| o.labels.keys().cloned())
            .collect();
        Self {
            occupants,
            supplied,
            unattributed: AtomicUsize::new(0),
        }
    }

    /// See [`Occupants::skipped_before_full`].
    pub fn skipped_before_full(&self) -> usize {
        self.occupants.skipped_before_full
    }

    /// Samples of slot columns that had no occupant on record, so far.
    pub fn unattributed(&self) -> usize {
        self.unattributed.load(Ordering::Relaxed)
    }

    /// The slot a column stands for, if the index describes it: its `id`
    /// label, the member index the agent stamps on every group member. A
    /// column without one is not a slot — a group can carry a plain member
    /// — and a slot the index never named has nothing to say about it;
    /// either is read under its own labels.
    fn slot_of(&self, labels: &Labels) -> Option<u32> {
        let slot: u32 = labels.inner.get("id")?.parse().ok()?;
        (!self.occupants.spans(slot).is_empty()).then_some(slot)
    }

    /// The column's labels with the occupant's laid over them.
    fn overlay(base: &Labels, occupant: &BTreeMap<String, String>) -> Labels {
        let mut out = base.clone();
        for (k, v) in occupant {
            out.inner.insert(k.clone(), v.clone());
        }
        out
    }
}

impl ColumnRelabel for OccupantRelabel {
    fn identities(&self, _name: &str, labels: &Labels) -> Option<Vec<Labels>> {
        let slot = self.slot_of(labels)?;
        let mut out: Vec<Labels> = Vec::new();
        for span in self.occupants.spans(slot) {
            let l = Self::overlay(labels, &span.labels);
            if !out.contains(&l) {
                out.push(l);
            }
        }
        Some(out)
    }

    /// Runs follow the slot's spans: one binary search per span boundary,
    /// the overlay computed once per run rather than once per sample.
    fn split(&self, _name: &str, labels: &Labels, timestamps: &[u64]) -> Option<Vec<Run>> {
        let slot = self.slot_of(labels)?;
        let spans = self.occupants.spans(slot);
        let n = timestamps.len();
        let mut runs: Vec<Run> = Vec::new();
        let mut i = 0;
        let mut si = 0;
        while i < n {
            let ts = timestamps[i];
            while si < spans.len() && spans[si].to.is_some_and(|to| to <= ts) {
                si += 1;
            }
            let (run_labels, until) = match spans.get(si) {
                Some(span) if span.from <= ts => (Self::overlay(labels, &span.labels), span.to),
                // In a gap before the next span, or past the last one.
                next => (labels.clone(), next.map(|s| s.from)),
            };
            let j = match until {
                Some(end) => i + timestamps[i..].partition_point(|t| *t < end),
                None => n,
            };
            if run_labels == *labels {
                self.unattributed.fetch_add(j - i, Ordering::Relaxed);
            }
            runs.push((run_labels, i..j));
            i = j;
        }
        Some(runs)
    }

    fn at(&self, _name: &str, labels: &Labels, timestamp: u64) -> Option<Labels> {
        let slot = self.slot_of(labels)?;
        Some(match self.occupants.at(slot, timestamp) {
            Some(o) => Self::overlay(labels, &o.labels),
            None => {
                self.unattributed.fetch_add(1, Ordering::Relaxed);
                labels.clone()
            }
        })
    }

    /// A filter on keys the index supplies becomes a filter on the slots
    /// whose occupants match: `comm="redis"` asks the segment for
    /// `id="3|17"`, and the reader applies `comm="redis"` to the relabelled
    /// runs afterwards. Without this the segment would decode every column
    /// of the metric — or, worse, match none, since a filter key absent from
    /// the column's labels fails closed.
    fn segment_filter(&self, _name: &str, filter: &Labels) -> Labels {
        let (ours, theirs): (BTreeMap<String, String>, BTreeMap<String, String>) = filter
            .inner
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .partition(|(k, _)| self.supplied.contains(k));
        if ours.is_empty() {
            return filter.clone();
        }
        let ours = Labels { inner: ours };
        let mut out = Labels { inner: theirs };
        let id_filter = out.inner.remove("id");
        let slots: Vec<String> = self
            .occupants
            .slots()
            .filter(|slot| {
                self.occupants.spans(*slot).iter().any(|span| {
                    Labels {
                        inner: span.labels.clone(),
                    }
                    .matches(&ours)
                })
            })
            .map(|slot| slot.to_string())
            .filter(|slot| match &id_filter {
                Some(existing) => Labels {
                    inner: [("id".to_string(), slot.clone())].into_iter().collect(),
                }
                .matches(&Labels {
                    inner: [("id".to_string(), existing.clone())].into_iter().collect(),
                }),
                None => true,
            })
            .collect();
        // No slot can match: a value no `id` carries, so the segment answers
        // nothing rather than everything.
        let alternation = if slots.is_empty() {
            "(none)".to_string()
        } else {
            slots.join("|")
        };
        out.inner.insert("id".to_string(), alternation);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryKind, IndexState, SlotEntry};

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn entry(kind: EntryKind, slots: &[(u32, &[(&str, &str)])], removed: &[u32]) -> Vec<u8> {
        IndexEntry {
            kind,
            slots: slots
                .iter()
                .map(|(slot, l)| SlotEntry {
                    slot: *slot,
                    labels: labels(l),
                })
                .collect(),
            removed: removed.to_vec(),
            state: IndexState::default(),
        }
        .encode()
    }

    #[test]
    fn a_reassigned_slot_is_two_occupants_split_at_the_entry() {
        let entries = vec![
            (
                100,
                entry(EntryKind::Full, &[(3, &[("comm", "redis")])], &[]),
            ),
            (
                250,
                entry(EntryKind::Delta, &[(3, &[("comm", "valkey")])], &[]),
            ),
        ];
        let occ = Occupants::replay(&entries).unwrap();
        assert_eq!(
            occ.spans(3),
            &[
                Occupancy {
                    from: 100,
                    to: Some(250),
                    labels: labels(&[("comm", "redis")])
                },
                Occupancy {
                    from: 250,
                    to: None,
                    labels: labels(&[("comm", "valkey")])
                },
            ]
        );
        assert_eq!(
            occ.at(3, 99),
            None,
            "before the first entry nothing is known"
        );
        assert_eq!(
            occ.at(3, 249).map(|o| &o.labels),
            Some(&labels(&[("comm", "redis")]))
        );
        assert_eq!(
            occ.at(3, 250).map(|o| &o.labels),
            Some(&labels(&[("comm", "valkey")])),
            "the row stamped as the entry already carries the new occupant"
        );
    }

    #[test]
    fn a_removed_slot_has_no_occupant_until_it_is_filled_again() {
        let entries = vec![
            (
                100,
                entry(EntryKind::Full, &[(3, &[("comm", "redis")])], &[]),
            ),
            (200, entry(EntryKind::Delta, &[], &[3])),
            (
                300,
                entry(EntryKind::Delta, &[(3, &[("comm", "nginx")])], &[]),
            ),
        ];
        let occ = Occupants::replay(&entries).unwrap();
        assert_eq!(
            occ.at(3, 150).map(|o| o.labels["comm"].as_str()),
            Some("redis")
        );
        assert_eq!(
            occ.at(3, 250),
            None,
            "empty between the removal and the refill"
        );
        assert_eq!(
            occ.at(3, 300).map(|o| o.labels["comm"].as_str()),
            Some("nginx")
        );
    }

    /// A `Full` names the whole set: a slot it leaves out ended, even though
    /// no `removed` says so.
    #[test]
    fn a_full_closes_the_slots_it_does_not_name() {
        let entries = vec![
            (
                100,
                entry(
                    EntryKind::Full,
                    &[(1, &[("cpu", "1")]), (2, &[("cpu", "2")])],
                    &[],
                ),
            ),
            (200, entry(EntryKind::Full, &[(1, &[("cpu", "1")])], &[])),
        ];
        let occ = Occupants::replay(&entries).unwrap();
        assert_eq!(
            occ.spans(1).len(),
            1,
            "unchanged across the resend: one span"
        );
        assert_eq!(occ.spans(1)[0].to, None);
        assert_eq!(occ.spans(2)[0].to, Some(200));
    }

    #[test]
    fn a_delta_before_any_full_is_skipped_and_counted() {
        let entries = vec![
            (
                100,
                entry(EntryKind::Delta, &[(3, &[("comm", "lost")])], &[]),
            ),
            (
                200,
                entry(EntryKind::Full, &[(3, &[("comm", "redis")])], &[]),
            ),
        ];
        let occ = Occupants::replay(&entries).unwrap();
        assert_eq!(occ.skipped_before_full, 1);
        assert_eq!(occ.at(3, 150), None);
        assert_eq!(
            occ.at(3, 200).map(|o| o.labels["comm"].as_str()),
            Some("redis")
        );
    }

    #[test]
    fn a_span_opened_and_closed_at_one_timestamp_is_not_kept() {
        let entries = vec![
            (100, entry(EntryKind::Full, &[(3, &[("comm", "a")])], &[])),
            (100, entry(EntryKind::Delta, &[(3, &[("comm", "b")])], &[])),
        ];
        let occ = Occupants::replay(&entries).unwrap();
        assert_eq!(occ.spans(3).len(), 1);
        assert_eq!(occ.spans(3)[0].labels["comm"], "b");
    }

    fn lb(pairs: &[(&str, &str)]) -> Labels {
        Labels {
            inner: labels(pairs),
        }
    }

    fn handover() -> OccupantRelabel {
        let entries = vec![
            (
                100,
                entry(
                    EntryKind::Full,
                    &[(3, &[("comm", "redis")]), (4, &[("comm", "nginx")])],
                    &[],
                ),
            ),
            (
                250,
                entry(EntryKind::Delta, &[(3, &[("comm", "valkey")])], &[]),
            ),
        ];
        OccupantRelabel::new(Occupants::replay(&entries).unwrap())
    }

    #[test]
    fn a_slot_column_presents_as_each_of_its_occupants() {
        let r = handover();
        let col = lb(&[("id", "3"), ("metric_kind", "x")]);
        assert_eq!(
            r.identities("m", &col),
            Some(vec![
                lb(&[("id", "3"), ("metric_kind", "x"), ("comm", "redis")]),
                lb(&[("id", "3"), ("metric_kind", "x"), ("comm", "valkey")]),
            ])
        );
        assert_eq!(
            r.identities("m", &lb(&[("plain", "1")])),
            None,
            "not a slot"
        );
        assert_eq!(
            r.identities("m", &lb(&[("id", "9")])),
            None,
            "a slot the index never named is read as it is"
        );
        assert_eq!(r.split("m", &lb(&[("id", "9")]), &[100, 200]), None);
    }

    #[test]
    fn a_columns_samples_are_cut_at_the_handover() {
        let r = handover();
        let col = lb(&[("id", "3")]);
        let runs = r.split("m", &col, &[100, 150, 200, 250, 300]).unwrap();
        assert_eq!(
            runs,
            vec![
                (lb(&[("id", "3"), ("comm", "redis")]), 0..3),
                (lb(&[("id", "3"), ("comm", "valkey")]), 3..5),
            ]
        );
        assert_eq!(r.unattributed(), 0);
        assert_eq!(
            r.at("m", &col, 249),
            Some(lb(&[("id", "3"), ("comm", "redis")]))
        );
        assert_eq!(
            r.at("m", &col, 250),
            Some(lb(&[("id", "3"), ("comm", "valkey")]))
        );
    }

    /// Samples before the first entry have no occupant: they keep the
    /// column's labels and are counted.
    #[test]
    fn samples_with_no_occupant_keep_the_columns_labels_and_are_counted() {
        let r = handover();
        let col = lb(&[("id", "3")]);
        let runs = r.split("m", &col, &[50, 75, 100]).unwrap();
        assert_eq!(
            runs,
            vec![
                (col.clone(), 0..2),
                (lb(&[("id", "3"), ("comm", "redis")]), 2..3),
            ]
        );
        assert_eq!(r.unattributed(), 2);
    }

    /// A filter on an index-supplied key is turned into the slots whose
    /// occupants match, so the segment decodes those columns only.
    #[test]
    fn a_filter_on_an_index_label_becomes_a_slot_filter() {
        let r = handover();
        assert_eq!(
            r.segment_filter("m", &lb(&[("comm", "valkey")])),
            lb(&[("id", "3")])
        );
        assert_eq!(
            r.segment_filter("m", &lb(&[("comm", "~redis|nginx")])),
            lb(&[("id", "3|4")])
        );
        assert_eq!(
            r.segment_filter("m", &lb(&[("comm", "postgres")])),
            lb(&[("id", "(none)")]),
            "no slot ever held it: match nothing, not everything"
        );
        // Keys the columns carry pass through; an `id` the query already
        // pinned is intersected.
        assert_eq!(
            r.segment_filter("m", &lb(&[("comm", "nginx"), ("id", "4"), ("host", "a")])),
            lb(&[("id", "4"), ("host", "a")])
        );
        assert_eq!(
            r.segment_filter("m", &lb(&[("comm", "nginx"), ("id", "3")])),
            lb(&[("id", "(none)")])
        );
        assert_eq!(
            r.segment_filter("m", &lb(&[("host", "a")])),
            lb(&[("host", "a")]),
            "nothing of ours: unchanged"
        );
    }
}
