//! The `.rez` v3 write-ahead log: the row format rows land in as they arrive,
//! and the materialization that turns a live WAL tail back into a parquet
//! segment.
//!
//! **Split out of the writer deliberately.** Materializing the tail is a READ
//! operation — every reader of a live archive does it, including one in a
//! browser — so it must not sit behind the writer's dependencies. Nothing here
//! needs `metriken-exposition` except the one ingest-side conversion, which is
//! gated on `write`.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[cfg(feature = "write")]
use crate::rez::Entry;
use crate::rez::{write_table_parquet, Cell, CellValue, GroupTableBuilder, TableBuilder};
use crate::rez_sqlite::WalRow;
use crate::window::Window;

/// One metric's contribution to a WAL row: exactly what
/// `TableBuilder::push_row` needs to place the value in its column, and nothing
/// else. The recorder's own `Snapshot` entry carries a good deal more, and
/// carrying it per tick would cost several times the payload for information
/// that does not change between ticks.
///
/// Encoded with `rmp_serde::to_vec`, which writes structs as ARRAYS and enums
/// as `[index, payload]`, so these field names cost nothing on the wire and are
/// chosen for readability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalCell {
    /// The snapshot entry's name — the segment's column key (`"5"`, `"5x3"`).
    /// Numeric-id strings of a few bytes, so carrying one per cell per tick is
    /// noise next to the value; dropping them and relying on positional order
    /// would not be, because cgroup metrics appear and vanish mid-recording and
    /// a positional decode would silently reattribute every later column.
    pub name: String,
    /// The snapshot **entry's** metadata, verbatim — NOT the parquet column's.
    ///
    /// The difference matters to a reader. `metric_type` is **not** in here:
    /// `TableBuilder::push_row` injects it (`rez.rs`, the `or_insert_with` that
    /// builds a `RezColumn`) and `metriken-exposition` never carries it. A
    /// recovery path that built `RezColumn { metadata: cell.metadata, .. }`
    /// directly would produce a column a natively sealed segment does not
    /// match, and `read_table_parquet` would then read every gauge back as a
    /// counter. Derive `metric_type` from the [`WalValue`] tag — or, simplest
    /// and what makes the two paths identical by construction, rebuild owned
    /// `Counter`/`Gauge`/`Histogram` entries and replay them through
    /// `TableBuilder::push_row`, which injects it exactly as the writer did.
    /// (A histogram's `grouping_power`/`max_value_power` DO appear here, put
    /// there by the agent's exposition; [`WalValue::Histogram`] carries them
    /// too, so a cell decodes without consulting metadata at all.)
    ///
    /// Carried ONLY on the first WAL row in which this metric appears **in the
    /// current segment** — `maybe_seal` clears the tracking for a sampler when
    /// it seals, so each segment's WAL span re-anchors its own metadata.
    ///
    /// Repeating it every tick is exactly the full-msgpack cost values-only
    /// rows exist to avoid; re-anchoring costs one payload per metric per
    /// *segment*, i.e. roughly one tick in `max_rows`. What that buys is an
    /// invariant contained entirely in the live WAL: **the
    /// first live WAL row mentioning a metric carries its metadata.** No
    /// segment lookup, so no decoding an arbitrarily old segment footer to
    /// learn a tail's labels — the cost the WAL exists to avoid — and nothing
    /// breaks when hindsight retention deletes old segments
    /// (`DELETE FROM segments WHERE last_ts < cutoff`).
    ///
    /// It also makes the WAL's metadata semantics *identical* to a segment
    /// column's, which an anchor held for the recording's lifetime did not:
    /// `seal_completed` installs a fresh `TableBuilder` at every rotation, so a
    /// column re-latches its labels each segment. A metric whose labels drift
    /// mid-recording (a unit correction, an agent restart remapping an id) is
    /// therefore captured in the WAL exactly where it is captured in segments.
    /// And the tracking set no longer grows without bound as cgroup metric
    /// names churn.
    pub metadata: Option<BTreeMap<String, String>>,
    pub value: WalValue,
    /// The acquisition window, as `(begin_ns, end_ns)`.
    pub window: Option<(u64, u64)>,
}

/// A cell's value, tagged by shape — which is also what tells a reader which
/// `RezValues` column the cell belongs in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WalValue {
    Counter(u64),
    Gauge(i64),
    /// `(grouping_power, max_value_power, buckets)`. The H2 config travels with
    /// the buckets so `histogram::Histogram::from_buckets` needs nothing else —
    /// two bytes against a 7,424-bucket payload, and it keeps the cell decodable
    /// without consulting the metadata row.
    Histogram(u8, u8, Vec<u64>),
}

impl WalValue {
    /// The ingest boundary for a cell's value: a snapshot entry becomes the
    /// WAL's own representation on the way in.
    #[cfg(feature = "write")]
    pub fn of(entry: &Entry<'_>) -> Self {
        match entry {
            Entry::Counter(c) => WalValue::Counter(c.value),
            Entry::Gauge(g) => WalValue::Gauge(g.value),
            Entry::Histogram(h) => WalValue::Histogram(
                h.value.config().grouping_power(),
                h.value.config().max_value_power(),
                h.value.as_slice().to_vec(),
            ),
        }
    }
}

/// Encode a sampler's live WAL rows as one parquet segment — `None` when there
/// is no tail.
///
/// **This replays the rows through `TableBuilder::push_row`, the same call
/// `ingest` makes, rather than assembling columns directly.** That is not
/// stylistic. `WalCell::metadata` is the snapshot *entry's* metadata and does
/// not carry `metric_type` — `push_row` injects it. A tail built by copying
/// that metadata into a `RezColumn` yields a segment a natively sealed one
/// does not match, and `read_table_parquet` then reads every gauge back as a
/// counter. Going through the writer's own call makes the two shapes identical
/// by construction instead of by careful duplication.
///
/// Metadata is carried only on the first WAL row in which a metric appears in
/// the current segment's WAL span, and `push_row` reads a column's metadata
/// only when it first creates that column — so passing each cell's metadata
/// through verbatim is exactly right: the first mention establishes the
/// column, later mentions are ignored.
fn materialize_sampler_wal_tail(
    sampler: &str,
    rows: &[WalRow],
) -> Result<Option<MaterializedTail>, Box<dyn std::error::Error>> {
    if rows.is_empty() {
        return Ok(None);
    }
    // Never skips a row (unlike the group path, below) — every row in `rows`
    // ends up in the materialized table, so its extent IS `rows`' own span.
    let first_ts = rows[0].ts;
    let row_count = rows.len() as u64;
    let mut builder = TableBuilder::new(sampler.to_string());
    for row in rows {
        // Decoded once into owned parts, then borrowed as `Cell`s in the
        // cells' original order — column order is `push_row`'s insertion
        // order, so preserving it is what keeps a materialized segment's
        // schema in the same order a natively sealed one has.
        let decoded = decode_wal_row(&row.row)?;
        let mut names: Vec<String> = Vec::with_capacity(decoded.len());
        let mut metas: Vec<HashMap<String, String>> = Vec::with_capacity(decoded.len());
        let mut windows: Vec<Option<Window>> = Vec::with_capacity(decoded.len());
        // Scalars are copied out; histograms are rebuilt (and thereby
        // VALIDATED) into a side vector the cells borrow from.
        let mut scalars: Vec<Option<WalValue>> = Vec::with_capacity(decoded.len());
        let mut hists: Vec<Option<histogram::Histogram>> = Vec::with_capacity(decoded.len());
        for cell in decoded {
            names.push(cell.name.clone());
            metas.push(
                cell.metadata
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
            );
            windows.push(cell.window.map(|(begin, end)| Window::new(begin, end)));
            match cell.value {
                WalValue::Histogram(grouping_power, max_value_power, buckets) => {
                    // The H2 config travels with the buckets, so nothing has to
                    // be recovered from the metadata row.
                    let h = histogram::Histogram::from_buckets(
                        grouping_power,
                        max_value_power,
                        buckets,
                    )
                    .map_err(|e| {
                        format!(
                            "failed to rebuild the {sampler} histogram {}: {e}",
                            cell.name
                        )
                    })?;
                    scalars.push(None);
                    hists.push(Some(h));
                }
                v => {
                    scalars.push(Some(v));
                    hists.push(None);
                }
            }
        }
        let cells: Vec<Cell<'_>> = (0..names.len())
            .map(|i| Cell {
                name: &names[i],
                metadata: &metas[i],
                window: windows[i],
                value: match (&scalars[i], &hists[i]) {
                    (Some(WalValue::Counter(v)), _) => CellValue::Counter(*v),
                    (Some(WalValue::Gauge(v)), _) => CellValue::Gauge(*v),
                    (_, Some(h)) => CellValue::Histogram(h),
                    // `scalars[i]` and `hists[i]` are filled as a pair above:
                    // exactly one of them is `Some` for every index.
                    _ => unreachable!("every decoded WAL cell is a scalar or a histogram"),
                },
            })
            .collect();
        builder.push_row(row.ts, row.wall_offset, &cells);
    }
    Ok(Some(MaterializedTail {
        bytes: write_table_parquet(&builder.finish())?,
        rows: row_count,
        first_ts,
    }))
}

/// A materialized segment's bytes plus the actual extent of INPUT rows that
/// went into it — which can differ from the caller's own `rows` slice for a
/// V3 group table whose leading rows were skipped as un-anchored (see
/// `materialize_group_wal_tail`). `materialize_sampler_wal_tail` never
/// skips, so its `rows`/`first_ts` are always the input slice's own span —
/// this type exists so both paths report the same two facts uniformly and a
/// caller (`seal_batch`) never has to know which one ran.
///
/// **`last_ts` is deliberately NOT here.** Unlike `first_ts`/`rows`, the
/// input slice's OWN last row's timestamp is always correct as a segment's
/// `last_ts` even when leading rows were skipped: a V3 group's un-anchored
/// run is always a LEADING prefix (retention removes a prefix, never punches
/// a hole — `RezDb::evict_before`'s doc), so the last input row is never
/// itself skipped. Callers already have that timestamp from the `WalRow`s
/// they read; duplicating it here would just be a second place for it to
/// drift from the one that is actually used.
#[derive(Debug, PartialEq)]
pub struct MaterializedTail {
    pub bytes: Vec<u8>,
    pub rows: u64,
    pub first_ts: u64,
}

/// True for a V3 acquisition-group table key (`"<sampler>/<group>"`); false
/// for a V1/V2 sampler table key, which never contains `/` for every
/// REGISTERED sampler of this build (see `group_by_sampler`'s `sampler_of`
/// and `no_registered_sampler_name_contains_a_slash`, below). `sampler_of`
/// itself reads the `"sampler"` metadata key straight off the wire,
/// unvalidated — a hostile or merely unusual endpoint could in principle
/// send a value containing `/` — which is exactly why this convention is
/// backed by more than good naming: see the fail-closed backstop below.
///
/// This is the ONLY discriminator available to [`materialize_wal_tail`]: a
/// WAL row is an opaque BLOB keyed only by this string (see the WAL-key
/// design note on [`StreamRecorderV3`]), so there is nowhere else to look —
/// no separate "table kind" column, and a fresh reader process (`rez_reader.rs`
/// opening a `.rez` some other process is still writing) has no in-memory
/// state from the writer to consult either. It is safe because the two
/// row shapes cannot be mistaken for one another even if this guess were
/// wrong: `decode_wal_group_row`/`decode_wal_row` decode structurally
/// different msgpack shapes (a `WalGroupRow` struct vs. an array of
/// `WalCell`s) and error rather than silently misinterpreting the bytes.
///
/// The convention itself is enforced at debug build time by a
/// `debug_assert!` at each end (`ingest`'s V1/V2 loop and `ingest_v3`'s
/// group loop, both in `StreamRecorderV3`) plus
/// `no_registered_sampler_name_contains_a_slash` pinning the invariant
/// against every registered `SAMPLERS` entry; the structural non-aliasing
/// above is the release-build backstop if that is ever violated anyway.
pub fn is_group_table_key(table_key: &str) -> bool {
    table_key.contains('/')
}

/// Encode a `.rez` table's live WAL rows as one parquet segment — dispatches
/// on [`is_group_table_key`] to the V3 group-row path or the V1/V2
/// sampler-cell path. `None` when there is no tail.
///
/// Both the writer thread (`seal_batch`) and a completely independent reader
/// process (`rez_reader.rs`, opening a `.rez` some other process is still
/// writing) call this — neither has access to `StreamRecorderV3`'s in-memory
/// schema cache, which is why a V3 group's WAL rows must be self-sufficient
/// (see [`WalGroupRow`]).
pub fn materialize_wal_tail(
    table_key: &str,
    rows: &[WalRow],
) -> Result<Option<MaterializedTail>, Box<dyn std::error::Error>> {
    if is_group_table_key(table_key) {
        materialize_group_wal_tail(table_key, rows)
    } else {
        materialize_sampler_wal_tail(table_key, rows)
    }
}

/// One V3 acquisition-group's WAL payload for one tick: values + ONE shared
/// window, with member names/metadata resolved from a schema rather than
/// carried per cell — the WAL row shrinks to values + one window, per the
/// schema-hash cache design (see `StreamRecorderV3`'s `schemas` field).
///
/// **Self-sufficiency, not just bandwidth.** `schema` is `Some` only on the
/// row that (re-)anchors this group's schema for the segment currently
/// accumulating in this table's live WAL — mirroring `WalCell::metadata`'s
/// "first mention in this segment" rule, at group granularity instead of
/// per-metric (`StreamRecorderV3`'s `segment_schema` map decides this,
/// independently of whether the AGENT'S payload included a schema this
/// tick). `schema_hash` is always present so a decoder can tell schema drift
/// from steady state even when `schema` is `None`. This is what lets
/// `materialize_wal_tail` rebuild a group table from WAL rows ALONE, with no
/// external schema cache — required because both the writer thread and a
/// fresh reader process call it (see `materialize_wal_tail`'s doc).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalGroupRow {
    /// Which schema (by content hash) these values align with.
    pub schema_hash: (u64, u64),
    /// The schema itself, present only on the row that (re-)anchors it —
    /// see the struct doc. `None` means "same schema as the nearest earlier
    /// row in this table's live WAL span."
    pub schema: Option<crate::schema::GroupSchema>,
    pub window: Option<(u64, u64)>,
    pub counters: Vec<Option<u64>>,
    pub gauges: Vec<Option<i64>>,
    /// `(grouping_power, max_value_power, buckets)` per histogram slot — the
    /// same shape `WalValue::Histogram` carries.
    pub histograms: Vec<Option<(u8, u8, Vec<u64>)>>,
}

pub fn encode_wal_group_row(row: &WalGroupRow) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec(row).map_err(|e| format!("failed to encode a group WAL row: {e}"))
}

/// The inverse of [`encode_wal_group_row`].
pub fn decode_wal_group_row(bytes: &[u8]) -> Result<WalGroupRow, String> {
    rmp_serde::from_slice(bytes).map_err(|e| format!("failed to decode a group WAL row: {e}"))
}

/// Encode a V3 acquisition-group's live WAL rows as one parquet segment —
/// `None` when there is no tail (including a tail every row of which had to
/// be skipped — see below). See [`WalGroupRow`] for why a decode walk needs
/// no external schema cache: it carries the current schema forward across
/// rows, normally requiring a `schema: Some` row before the first row that
/// needs it (an invariant `StreamRecorderV3::ingest_v3` upholds by always
/// anchoring a group's very first WAL row) — "normally" because retention
/// can delete that anchor out from under a still-live span (see below).
///
/// **Un-anchored rows degrade, they do not fail the recording.** Hindsight
/// retention (`RezDb::evict_before`) deletes WAL rows purely by `ts <
/// cutoff`, with no awareness of which row anchors a group's schema — a
/// `duration` under the seal policy's `max_age` (300s default) can delete a
/// group's anchor row while its later, still-live rows survive. Erroring
/// here on the resulting `schema: None` row with no matching anchor would
/// propagate through `seal_batch` and kill the writer thread — a live,
/// still-recording hindsight buffer going instantly and permanently
/// unreadable over a retention/seal-cadence interaction, not a corrupt
/// input. V1/V2 has no equivalent failure mode here (a column simply
/// rebuilds with whatever metadata its own surviving WAL span re-anchors),
/// so V3 matches that degrade-not-die posture: an un-anchored row — no
/// current anchor, or a hash that does not match the current one (the same
/// symptom a multi-anchor eviction gap would produce) — is skipped with a
/// rate-limited warning rather than erroring, and materialization resumes
/// from the next row that DOES carry a resolvable schema. A tail with no
/// resolvable row at all yields `None`, the same as an empty tail.
fn materialize_group_wal_tail(
    table_key: &str,
    rows: &[WalRow],
) -> Result<Option<MaterializedTail>, Box<dyn std::error::Error>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut builder = GroupTableBuilder::new(table_key.to_string());
    let mut current: Option<((u64, u64), crate::schema::GroupSchema)> = None;
    let mut warned_unanchored = false;
    // The ts of the first row actually pushed — `None` until then. This is
    // what makes a catalog `SegmentMeta::first_ts` correct even when a
    // leading un-anchored run was skipped: it is NOT `rows[0].ts` (the raw
    // WAL span's own start) unless nothing was skipped.
    let mut first_ts: Option<u64> = None;
    for row in rows {
        let decoded = decode_wal_group_row(&row.row)?;
        let schema = match decoded.schema {
            Some(s) => {
                current = Some((decoded.schema_hash, s));
                &current.as_ref().unwrap().1
            }
            None => match &current {
                Some((hash, s)) if *hash == decoded.schema_hash => s,
                _ => {
                    // No schema anchored yet, or the last anchor's hash does
                    // not match this row's — either way there is nothing to
                    // decode this row's values against. Skip it (and update
                    // no state), warning once per materialization so a
                    // retention-driven gap is visible without spamming.
                    if !warned_unanchored {
                        warn!(
                            "group {table_key} WAL tail row at ts={} has no matching schema \
                             anchor (likely evicted by retention); skipping until the next \
                             anchored row (warned once)",
                            row.ts
                        );
                        warned_unanchored = true;
                    }
                    continue;
                }
            },
        };
        let window = decoded.window.map(|(begin, end)| Window::new(begin, end));
        builder.push_row(
            row.ts,
            row.wall_offset,
            window,
            schema,
            &decoded.counters,
            &decoded.gauges,
            &decoded.histograms,
        );
        first_ts.get_or_insert(row.ts);
    }
    let row_count = builder.rows() as u64;
    if row_count == 0 {
        return Ok(None);
    }
    Ok(Some(MaterializedTail {
        bytes: write_table_parquet(&builder.finish())?,
        rows: row_count,
        // `row_count > 0` implies the loop pushed at least one row, which is
        // exactly when `first_ts` gets set — never `None` here.
        first_ts: first_ts.expect("a non-empty materialized table has a first pushed row"),
    }))
}

// Encode one sampler's cells for one tick into a `wal.row` BLOB.
pub fn encode_wal_row(cells: &[WalCell]) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec(cells).map_err(|e| format!("failed to encode a WAL row: {e}"))
}

/// The inverse of [`encode_wal_row`] — the recovery entry point.
pub fn decode_wal_row(bytes: &[u8]) -> Result<Vec<WalCell>, String> {
    rmp_serde::from_slice(bytes).map_err(|e| format!("failed to decode a WAL row: {e}"))
}
