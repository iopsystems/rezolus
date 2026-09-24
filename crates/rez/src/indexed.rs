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
//! time. This module replays the index into per-slot occupancy spans, decodes
//! the table's segments, and cuts each column into one series per occupant,
//! labelled with what the index says. The result is a
//! [`MemoryStore`](metriken_query::MemoryStore), which composes beside the
//! parquet-backed tables in the same union.
//!
//! The archive's rows do not change; only what they are attributed to. That
//! is why the split happens on read: the recorder writes what it received,
//! and the seam between two occupants of one slot is a fact of the index, not
//! of the values.
//!
//! # Where the labels come from
//!
//! A series' labels are the column's own field metadata (its storage keys
//! removed, exactly as the parquet loader does it) with the occupant's index
//! labels laid over the top. The index wins on a conflict. Today's archives
//! carry the same labels in both places — a column's metadata still holds the
//! occupant's labels and its `__uid__` — so the overlay changes nothing and
//! the two read paths agree series for series, which is what the oracle test
//! below pins. Once the writer stops copying identity into column metadata
//! (#1224 §2, step 7), the index is the only place the labels live, and this
//! path is the one that still knows them.
//!
//! A row with no occupant on record — before the stream's first `Full`, or a
//! slot the index never named — keeps the column's own labels and is counted
//! in [`IndexedTable::unattributed`]. Nothing is dropped.

use std::collections::{BTreeMap, HashMap};

use metriken_query::{is_storage_key, HistogramSnapshot, MemoryStore};

use crate::index::{EntryKind, IndexEntry};
use crate::rez::{read_table_parquet, RezColumn, RezTable, RezValues};

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

/// A table read through the index.
pub struct IndexedTable {
    /// The split series, as a source the union composes.
    pub store: MemoryStore,
    /// Series inserted.
    pub series: usize,
    /// Rows of slotted columns that had no occupant on record and kept the
    /// column's own labels.
    pub unattributed: usize,
    /// See [`Occupants::skipped_before_full`].
    pub skipped_before_full: usize,
}

/// The slot a column stands for.
///
/// The `id` metadata is the member index the agent stamps on every group
/// member, and the column name is `{metric_id}x{slot}` with an optional
/// `#generation` suffix the table builder adds when a slot was relabelled
/// within one segment (`GroupTableBuilder::get_or_create`). Either answers;
/// the name is the fallback for a column whose metadata was trimmed. A column
/// with neither is not a slot — a group can carry a plain member — and is
/// read under its own labels.
pub fn column_slot(column: &RezColumn) -> Option<u32> {
    if let Some(id) = column
        .metadata
        .get("id")
        .and_then(|v| v.parse::<u32>().ok())
    {
        return Some(id);
    }
    let name = column
        .name
        .rsplit_once('#')
        .filter(|(_, generation)| generation.parse::<u32>().is_ok())
        .map(|(base, _)| base)
        .unwrap_or(&column.name);
    let (metric_id, slot) = name.rsplit_once('x')?;
    metric_id.parse::<u64>().ok()?;
    slot.parse::<u32>().ok()
}

/// A series under assembly: one occupant of one column, or one unslotted
/// column, accumulated across segments.
struct SeriesBuild {
    timestamps: Vec<u64>,
    values: SeriesValues,
    /// One per sample. A `None` anywhere and the series is inserted without
    /// windows: the engine takes a window per sample or none at all.
    windows: Vec<Option<(u64, u64)>>,
}

enum SeriesValues {
    Counter(Vec<u64>),
    Gauge(Vec<i64>),
    Histogram(histogram::Config, Vec<HistogramSnapshot>),
}

/// Series identity while assembling: the name, the labels, and for a
/// histogram its bucket configuration, since two configurations cannot share
/// one series (the parquet reader splits them into `__run__` series, and so
/// does this).
type SeriesKey = (String, BTreeMap<String, String>, Option<(u8, u8)>);

/// Split `segments` — a table's sealed segments then its WAL tail, oldest
/// first — by the occupants `entries` describe, into a store.
///
/// `interval_ms` is the table's cadence, which the store reports as its own.
pub fn build(
    table_key: &str,
    segments: &[Vec<u8>],
    entries: &[(u64, Vec<u8>)],
    interval_ms: u64,
) -> Result<IndexedTable, String> {
    let occupants = Occupants::replay(entries)?;
    let mut series: Vec<(SeriesKey, SeriesBuild)> = Vec::new();
    let mut by_key: HashMap<SeriesKey, usize> = HashMap::new();
    let mut sample_timestamps: Vec<u64> = Vec::new();
    let mut unattributed = 0usize;

    for bytes in segments {
        let table = read_table_parquet(table_key.to_string(), bytes.clone())
            .map_err(|e| format!("decoding a segment of {table_key}: {e}"))?;
        sample_timestamps.extend_from_slice(&table.timestamps);
        for column in &table.columns {
            let name = column
                .metadata
                .get("metric")
                .cloned()
                .unwrap_or_else(|| column.name.clone());
            let base_labels: BTreeMap<String, String> = column
                .metadata
                .iter()
                .filter(|(k, _)| !is_storage_key(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let slot = column_slot(column);
            let config = match &column.values {
                RezValues::Histogram(hs) => hs
                    .iter()
                    .flatten()
                    .next()
                    .map(|h| (h.config().grouping_power(), h.config().max_value_power())),
                _ => None,
            };

            for row in 0..table.timestamps.len() {
                if !has_value(column, row) {
                    continue;
                }
                let ts = table.timestamps[row];
                let occupant = slot.and_then(|s| occupants.at(s, ts));
                let labels = match occupant {
                    Some(o) => {
                        let mut l = base_labels.clone();
                        l.extend(o.labels.iter().map(|(k, v)| (k.clone(), v.clone())));
                        l
                    }
                    None => {
                        if slot.is_some() {
                            unattributed += 1;
                        }
                        base_labels.clone()
                    }
                };
                let key = (name.clone(), labels, config);
                let idx = match by_key.get(&key) {
                    Some(i) => *i,
                    None => {
                        let values = match &column.values {
                            RezValues::Counter(_) => SeriesValues::Counter(Vec::new()),
                            RezValues::Gauge(_) => SeriesValues::Gauge(Vec::new()),
                            RezValues::Histogram(_) => {
                                let (gp, mvp) = config.expect("a present histogram has a config");
                                let config = histogram::Config::new(gp, mvp)
                                    .map_err(|e| format!("{name}: bucket config: {e}"))?;
                                SeriesValues::Histogram(config, Vec::new())
                            }
                        };
                        series.push((
                            key.clone(),
                            SeriesBuild {
                                timestamps: Vec::new(),
                                values,
                                windows: Vec::new(),
                            },
                        ));
                        by_key.insert(key, series.len() - 1);
                        series.len() - 1
                    }
                };
                let build = &mut series[idx].1;
                build.timestamps.push(ts);
                build.windows.push(row_window(&table, column, row));
                match (&mut build.values, &column.values) {
                    (SeriesValues::Counter(out), RezValues::Counter(v)) => {
                        out.push(v[row].expect("checked present"))
                    }
                    (SeriesValues::Gauge(out), RezValues::Gauge(v)) => {
                        out.push(v[row].expect("checked present"))
                    }
                    (SeriesValues::Histogram(_, out), RezValues::Histogram(v)) => {
                        out.push(snapshot(v[row].as_ref().expect("checked present")))
                    }
                    _ => unreachable!("a series keeps its column's kind"),
                }
            }
        }
    }

    // `__run__` for a histogram name that resolved to more than one bucket
    // configuration, numbered in first-seen order — the segmented reader's
    // policy, so the two paths name the same runs.
    let mut runs: HashMap<String, Vec<(u8, u8)>> = HashMap::new();
    for ((name, _, config), _) in &series {
        if let Some(c) = config {
            let v = runs.entry(name.clone()).or_default();
            if !v.contains(c) {
                v.push(*c);
            }
        }
    }

    let store = MemoryStore::builder()
        .sampling_interval_ms(interval_ms.max(1))
        .build();
    let count = series.len();
    for ((name, mut labels, config), build) in series {
        if let Some(c) = config {
            let configs = &runs[&name];
            if configs.len() > 1 {
                let run = configs.iter().position(|x| *x == c).expect("listed");
                labels.insert("__run__".to_string(), run.to_string());
            }
        }
        let windows = build
            .windows
            .iter()
            .copied()
            .collect::<Option<Vec<(u64, u64)>>>();
        match build.values {
            SeriesValues::Counter(values) => {
                store.insert_counter_series(&name, labels, build.timestamps, values, windows)?
            }
            SeriesValues::Gauge(values) => {
                store.insert_gauge_series(&name, labels, build.timestamps, values, windows)?
            }
            SeriesValues::Histogram(config, snapshots) => {
                store.insert_histogram_series(&name, labels, config, build.timestamps, snapshots)?
            }
        }
    }
    sample_timestamps.sort_unstable();
    sample_timestamps.dedup();
    store.set_sample_timestamps(sample_timestamps);

    Ok(IndexedTable {
        store,
        series: count,
        unattributed,
        skipped_before_full: occupants.skipped_before_full,
    })
}

fn has_value(column: &RezColumn, row: usize) -> bool {
    match &column.values {
        RezValues::Counter(v) => v.get(row).is_some_and(Option::is_some),
        RezValues::Gauge(v) => v.get(row).is_some_and(Option::is_some),
        RezValues::Histogram(v) => v.get(row).is_some_and(Option::is_some),
    }
}

/// The acquisition window of one sample: the column's own if it carries
/// them (a V2-shaped table), else the table's (a group table, one window per
/// row for every member).
fn row_window(table: &RezTable, column: &RezColumn, row: usize) -> Option<(u64, u64)> {
    // The decoder gives every column a windows vector the length of the
    // table, all `None` for a group table, so the column's entry is consulted
    // for a window and not for whether it has one.
    let w = column
        .windows
        .get(row)
        .copied()
        .flatten()
        .or_else(|| table.table_window.as_ref()?.get(row).copied().flatten());
    w.map(|w| (w.begin_ns, w.end_ns))
}

/// A histogram as the store holds it: the cumulative count at every
/// non-empty bucket. The same shape `MemoryStore::ingest_snapshot` builds.
fn snapshot(h: &histogram::Histogram) -> HistogramSnapshot {
    let mut index = Vec::new();
    let mut count = Vec::new();
    let mut running: u64 = 0;
    for (i, bucket) in h.iter().enumerate() {
        let c = bucket.count();
        if c > 0 {
            running = running.saturating_add(c);
            index.push(i as u32);
            count.push(running);
        }
    }
    HistogramSnapshot { index, count }
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

    fn column(name: &str, metadata: &[(&str, &str)]) -> RezColumn {
        RezColumn {
            name: name.to_string(),
            metadata: metadata
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            values: RezValues::Counter(Vec::new()),
            windows: Vec::new(),
        }
    }

    #[test]
    fn a_columns_slot_is_its_id_or_failing_that_its_name() {
        assert_eq!(column_slot(&column("17x4", &[("id", "4")])), Some(4));
        assert_eq!(column_slot(&column("17x4", &[])), Some(4), "from the name");
        assert_eq!(
            column_slot(&column("17x4#2", &[])),
            Some(4),
            "generation suffix ignored"
        );
        assert_eq!(
            column_slot(&column("17x4", &[("id", "9")])),
            Some(9),
            "metadata wins"
        );
        assert_eq!(column_slot(&column("17", &[])), None, "a plain member");
        assert_eq!(column_slot(&column("written", &[])), None);
        assert_eq!(
            column_slot(&column("boxx4", &[])),
            None,
            "the metric half is numeric"
        );
    }

    #[test]
    fn a_snapshot_is_cumulative_over_the_non_empty_buckets() {
        let mut h = histogram::Histogram::new(4, 12).unwrap();
        h.increment(1).unwrap();
        h.increment(1).unwrap();
        h.increment(100).unwrap();
        let s = snapshot(&h);
        assert_eq!(s.index.len(), 2);
        assert_eq!(s.count, vec![2, 3]);
    }
}
