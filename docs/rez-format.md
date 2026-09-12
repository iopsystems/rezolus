# The `.rez` archive format

This is the specification of what a `.rez` file *is*: the container, the
catalog, the encoding of every byte a reader has to interpret, and the rules
that keep two builds agreeing about them. It is written so that a reader or
writer could be built from it without `crates/rez`. Where the crate is the
ground truth for a detail, the path is cited; when the two disagree, the code
is what ships and this document has a bug.

Conventions version: **3** (`crates/rez/src/rez_sqlite.rs`,
`SCHEMA_VERSION`). See [Compatibility](#compatibility) for what changes it.

## 1. What a `.rez` holds

A `.rez` is a set of **recordings**. A recording is one metrics producer
observed over one span of time: for rezolus, one agent endpoint on one host.
Each recording holds **tables**, one per sampler or per acquisition group,
each recorded at its own cadence. A table is a sequence of immutable parquet
**segments** plus, while the recording is live, a tail of **WAL rows** not yet
sealed into a segment.

Three properties are the reason the format exists, and every rule below
serves one of them:

- **Valid at every instant.** The file is openable from the moment it is
  created; an unclean kill loses at most one tick per table.
- **Readable while written.** A reader sees a consistent snapshot and never
  blocks the writer.
- **Windows travel with values.** Every reading carries the interval over
  which it was acquired, so `rate()` can report a bound rather than a number.

## 2. Containers

Two containers exist. Detection is by content, never by filename
(`crates/rez/src/rez.rs`, `detect_rez_format`).

| Container | Detection | Status |
|---|---|---|
| **v3, SQLite** | Bytes `0..16` are `SQLite format 3\0` and the big-endian u32 at offset 68 (`application_id`) is `0x5245_5A00` (`REZ\0`) **or** `0` | Written by every current tool. |
| **v2/v1, tar** | A tar whose entries include `manifest.json` | Read-only. `rezolus recording upgrade` converts; `combine`/`filter`/`annotate` upgrade on the way in. |

An `application_id` of `0` is SQLite's default and is what every v3 archive
written before the stamp carried; such a file is accepted if it has the
catalog described below. Any other id is another application's database and
is not a `.rez`.

The rest of this document describes v3. The tar layout is documented by
`RezManifest` in `crates/rez/src/rez.rs` and is not extended.

### 2.1 SQLite geometry

Set at creation and persistent in the file (`RezDb::init_created`):

| Pragma | Value | Why |
|---|---|---|
| `page_size` | 4096 | Lowest per-tick WAL write amplification; measured, see the 2026-08-12 journal entry. |
| `auto_vacuum` | `INCREMENTAL` | Retention must not inflate a rolling buffer to its high-water mark. Cannot be enabled after the fact. |
| `journal_mode` | `WAL` | Readers never block the writer; commits are durable per tick. |
| `application_id` | `0x5245_5A00` | Format identity (see above). |
| `user_version` | `SCHEMA_VERSION` (3) | The conventions version, readable from the header. |

Per connection, not persistent: `synchronous=FULL` on writers,
`wal_autocheckpoint` denominated as 4 MiB of pages, and a `cache_size` that
differs for readers and writers. A reader that sets none of these still reads
correctly.

**The sidecar.** While a writer holds the file, SQLite keeps recent commits in
`<path>-wal` and an index in `<path>-shm`. A plain copy of `<path>` alone is a
valid SQLite file that may hold none of the recent ticks — and, before the
first checkpoint, no catalog at all. The writer checkpoints at least every
10 s (`rez_v3_writer::CHECKPOINT_INTERVAL`), so a plain copy is at most that
stale; `rezolus recording snapshot` (`VACUUM INTO`) is the exact copy.

## 3. The catalog

Five tables (`rez_sqlite.rs`, `SCHEMA_SQL`). SQLite is a transactional
allocator with a queryable catalog here, not a query engine: nothing below
ever looks inside a segment.

```sql
CREATE TABLE recordings(
  id INTEGER PRIMARY KEY,
  labels TEXT NOT NULL,               -- JSON object, string -> string
  metadata TEXT NOT NULL,             -- JSON object, string -> string
  complete INTEGER NOT NULL DEFAULT 0,
  clock_anchor_wall_ns INTEGER NOT NULL,
  uuid TEXT                            -- absent in archives before it existed
);
CREATE TABLE segments(
  recording_id INTEGER NOT NULL REFERENCES recordings(id),
  sampler TEXT NOT NULL,               -- the TABLE KEY, see §4
  seq INTEGER NOT NULL,
  rows INTEGER NOT NULL,
  first_ts INTEGER NOT NULL,
  last_ts INTEGER NOT NULL,
  bytes BLOB NOT NULL,                 -- one parquet file, §5
  PRIMARY KEY (recording_id, sampler, seq)
);
CREATE INDEX segments_by_time ON segments(recording_id, sampler, last_ts);
CREATE TABLE wal(
  recording_id INTEGER NOT NULL,
  sampler TEXT NOT NULL,
  ts INTEGER NOT NULL,
  wall_offset INTEGER NOT NULL,
  row BLOB NOT NULL,                   -- msgpack, §6
  PRIMARY KEY (recording_id, sampler, ts)
);
CREATE TABLE clock_offsets(
  recording_id INTEGER NOT NULL,
  ts INTEGER NOT NULL,
  offset_ns INTEGER NOT NULL
);
CREATE TABLE schema_version(version INTEGER NOT NULL);
```

### 3.1 `recordings`

- **`id`** is the rowid and is local to this file. It is renumbered by every
  copy and must not be used as an identity.
- **`uuid`** is the identity: a random v4 UUID in canonical `8-4-4-4-12`
  lowercase form, minted when the recording is inserted and carried verbatim
  by every copy (`combine`, `filter`, `annotate`, hindsight dump,
  `snapshot`). Two recordings with equal `uuid` are the same recording.
  `NULL` means unknown (the archive predates the column); a copy of such a
  recording mints a fresh uuid, so two copies are not claimed identical, only
  not known to differ.
- **`labels`** is the recording's *name*: an open string map used for
  selection (`--recording k=v`, `--baseline`/`--experiment`) and display. The
  recorder fills `source` and `host` and adds `record --label k=v`. Labels
  need not be unique across recordings; tools that need to name one refuse
  when they cannot.
- **`metadata`** is an open string map of everything else. Reserved keys are
  listed in §7.
- **`complete`** is `1` only after a clean finalize. `0` means data after the
  last row may be missing. Copies preserve it, except a hindsight dump, which
  is a complete artifact of a perpetually-live buffer and says so. A writer
  that reopens the archive and resumes the recording (§10.1) clears it, and
  its own finalize sets it again.
- **`clock_anchor_wall_ns`** pins the timeline, see §8.

### 3.2 `segments` and the table key

A table is identified by `(recording_id, sampler)`. The `sampler` column holds
the **table key**, which has two shapes:

- `<sampler>` — a whole sampler in one table. Produced from a V2 agent
  snapshot, or by parquet ingest.
- `<sampler>/<group>` — one acquisition group of a sampler. Produced from a
  V3 agent snapshot. A group is one read with one window (`docs/principles.md`
  §18), which is what lets the table carry a single window pair (§5.2).

**The `/` is the dispatch rule.** A key containing `/` is a group table and
its WAL rows decode as `WalGroupRow`; a key without one is a sampler table
and its WAL rows decode as `Vec<WalCell>` (§6). `table_sampler(key)` — the
text before the first `/` — is the unit `filter --samplers` and the manifest
listing work in. A sampler name must therefore never contain `/`, and a
writer that cannot guarantee that for its input must reject it.

Segments of one table are ordered by `seq`, dense from 0 in every copy
(copies renumber). `first_ts`/`last_ts` are the segment's row timestamps and
are what retention and range reads consult; `rows` is the row count. A
segment is immutable once inserted.

### 3.3 `wal`

One row per `(table, tick)`, keyed by the row timestamp. A WAL row is **live**
iff its `ts` is past the newest sealed row of its own table:

```sql
recording_id = ?1 AND sampler = ?2
  AND ts > COALESCE((SELECT MAX(last_ts) FROM segments
                     WHERE recording_id = ?1 AND sampler = ?2), 0)
```

(`rez_sqlite.rs`, `LIVE_WAL_PREDICATE`.) This is the recovery rule and the
reason a seal need not prune its rows in the same transaction: a row a
segment already covers is shadowed by the watermark whether or not it has
been deleted yet. A reader materializes the live rows of a table into one
in-memory parquet segment and appends it after the sealed ones. A table with
no sealed segment and live rows is a table, not an absence — a quiet sampler
early in a recording is exactly this.

### 3.4 `clock_offsets`

`(ts, offset_ns)` observations, one per seal batch and one at finalize: a
projection of the `:wall_offset` column (§5.1) at that batch's newest row.
Not guaranteed sorted. See §8.

### 3.5 `schema_version`

One row, the conventions version. Redundant with the header's `user_version`
since the stamp was introduced; kept because archives before the stamp have
only this. A reader checks the header when the id is stamped and this table
otherwise, and refuses a version it does not know.

## 4. Reading

The rules a reader must follow; `crates/rez/src/reader.rs` is the reference.

1. **Detect** by content (§2). Refuse a foreign `application_id`, a
   `user_version`/`schema_version` above the one you implement, and a v3 file
   with no `recordings` table (a copy taken from under a writer, §2.1).
2. **Read the catalog in one snapshot.** Every catalog question about a
   table — its segments, its live WAL rows, its span — must be answered from
   one `BEGIN DEFERRED` transaction. A seal committing between two
   autocommit reads inserts a segment the first read did not see and
   shadows the rows the second read would have returned; the seam then reads
   as a hole.
3. **A table's rows** are its segments in `seq` order followed by its
   materialized live WAL tail. The rule in §3.3 guarantees no duplicate row
   across the seam, so a reader does no de-duplication.
4. **A metric belongs to exactly one table** within a recording. A query
   naming metrics of several group tables of one sampler is answered by
   dispatching each metric to its own table (a union by name, no timestamp
   join); one naming two samplers, or the same sampler in two recordings, is
   refused.
5. **Never write.** A reader opens read-only in spirit even where the API
   does not enforce it; SQLite's last-connection-to-close checkpoint is the
   one side effect a read may have on a finished file.

## 5. Segment encoding

A segment is one parquet file (`crates/rez/src/rez.rs`, `table_to_batch`,
`segment_writer_props`): one row group per seal, `LZ4_RAW` compression,
dictionary encoding **off** (it dominated peak RSS on wide tables). Column
order is fixed and load-bearing for the reader's schema parse.

### 5.1 Every table

| Column | Arrow type | Field metadata | Meaning |
|---|---|---|---|
| `timestamp` | `UInt64`, non-null | `metric_type=timestamp`, `unit=nanoseconds` | Anchored row time, §8. Strictly increasing within a table. |
| `:wall_offset` | `Int64`, nullable | — | Wall-clock minus anchored time at this row, ns. Null where the table had no observation. |

### 5.2 Windows — two shapes, never both

A **group table** (key `<sampler>/<group>`) carries one window for the row,
immediately after `:wall_offset` and before any value column:

| Column | Arrow type | Meaning |
|---|---|---|
| `:window_begin` | `Int64`, nullable | `window.begin_ns − timestamp`, i.e. an offset, usually ≤ 0. |
| `:window_width` | `UInt64`, nullable | `window.end_ns − window.begin_ns`. |

A **sampler table** (key `<sampler>`) carries a pair per value column,
immediately after that column: `<name>:window_begin` and
`<name>:window_width`, same types and semantics. Null means that reading had
no window.

A writer errors if asked to emit both shapes in one table. A reader
recognizes the shape by whether a bare `:window_begin` field exists.

### 5.3 Value columns

Named by the producer's column key — for a rezolus agent, the metric's
numeric id as a string (`"5"`, or `"5x3"` for a per-slot metric) — with the
metric's identity in **field metadata**, not in the column name:

| Kind | Column name | Arrow type | Required field metadata |
|---|---|---|---|
| counter | `<key>` | `UInt64`, nullable | `metric` (the metric name), `metric_type=counter`, `sampler`, plus the producer's labels |
| gauge | `<key>` | `Int64`, nullable | as above, `metric_type=gauge` |
| histogram | `<key>:buckets` | `List<UInt64>`, nullable | as above, `metric_type=histogram`, `grouping_power`, `max_value_power` |

`metric_type` is injected by the writer if the producer omitted it; every
other key is the producer's metadata verbatim. A histogram's list is the
histogram's **full, dense** bucket-count array (`histogram::Histogram::as_slice`),
one `u64` per bucket; `grouping_power`/`max_value_power` are the H2
parameters that give each index its value range. Null in a value column
means "no reading this row" — a metric that appears mid-segment has nulls
before it.

A column's metadata is latched when the column first appears **in that
segment**; a segment is self-describing and never depends on an earlier one.

### 5.4 Names a reader must not treat as metrics

Anything beginning with `:`, anything ending in `:window_begin`,
`:window_width`, or `:buckets` (the last is a histogram's *storage* name, the
metric is the field's `metric`), and the `timestamp` column.

## 6. WAL row encoding

`wal.row` is msgpack produced by `rmp-serde` with its default struct encoding:
a struct is a positional **array**, an enum is `[variant_index, payload]`.
Field order is therefore the wire format, and the Rust declarations in
`crates/rez/src/wal.rs` and `crates/rez/src/schema.rs` are normative. Both
shapes are pinned byte-for-byte against the producer crate's types by tests.

### 6.1 Sampler table row: `Vec<WalCell>`

```
WalCell = [ name: str,
            metadata: Option<{str: str}>,     -- BTreeMap, sorted keys
            value: WalValue,
            window: Option<[begin_ns: u64, end_ns: u64]> ]
WalValue = [0, u64]                            -- Counter
         | [1, i64]                            -- Gauge
         | [2, [grouping_power: u8, max_value_power: u8, buckets: [u64]]]
```

`metadata` is carried **only on the first row in the current segment span
in which the metric appears**; later rows of the same span carry `None`. A
reader walking a span latches metadata forward. This is what makes a span
self-describing without an external catalog, and it is why retention (§9)
must not delete a span's first row out from under later ones.

### 6.2 Group table row: `WalGroupRow`

```
WalGroupRow = [ schema_hash: [hi: u64, lo: u64],
                schema: Option<GroupSchema>,
                window: Option<[begin_ns, end_ns]>,
                counters:   [Option<u64>],
                gauges:     [Option<i64>],
                histograms: [Option<[gp: u8, mvp: u8, buckets: [u64]]>] ]
GroupSchema = [ counters: [MetricDesc], gauges: [MetricDesc], histograms: [MetricDesc] ]
MetricDesc  = [ name: str, metadata: {str: str} ]
```

Values are positional against the schema. `schema` is present on the row
that (re-)anchors it — the first row of a segment span, and any row whose
`schema_hash` differs from the previous row's — and `None` otherwise,
meaning "same schema as the nearest earlier row in this span". The hash is
FNV-1a-128 over the schema's msgpack (`schema.rs`, `GroupSchema::hash`), the
same function the producer computes; a reader may use it to skip decoding an
unchanged schema but must never trust a `None` without an anchor.

## 7. Reserved metadata keys

`recordings.metadata` is open, but these keys have meaning to the tools:

| Key | Set by | Value |
|---|---|---|
| `source` | recorder | The endpoint's source name (`rezolus`, or `--endpoint …,source=NAME`). |
| `sampling_interval_ms` | recorder | The scrape interval. **Not** a per-table cadence promise; tables have their own. |
| `systeminfo` | recorder | JSON hardware summary from the agent. |
| `descriptions` | recorder | JSON map, metric name → help text. |
| `producer_epoch` | recorder, from the producer | The producer's **current counter epoch**: an opaque id the producer regenerates whenever its cumulative counters start from zero (for a rezolus agent, once per process). Two recordings with equal epochs over overlapping time observe **one** monotonic series — mergeable, never summable. Absent means unknown. |
| `producer_epochs` | recorder | JSON array `[{"epoch": id, "from_ts": ts}, …]`, every epoch this recording observed in order; the last is the current one. A length above 1 is a counter reset the reader can see rather than infer. |
| `writer_sessions` | writer | JSON array `[{"session": uuid, "clock_anchor_wall_ns": n, "resumed_after_ts": ts?}, …]`, one per writer session that appended to the recording, in order. One entry means the recording was written in one go; a later entry is a resume (§10.1), and its `clock_anchor_wall_ns` is *that* session's anchor. |
| `events` | recorder, writer, `annotate` | JSON `{"events": [Event, …]}`, `Event` as in `crates/dashboard/src/events.rs`. The recorder writes one per epoch change (`kind: "producer_epoch"`, `id: "producer_epoch:<new>"`); the writer writes one per resume (`kind: "writer_session"`, `id: "writer_session:<session>"`, at the new anchor). |
| `service_queries` | `annotate --queries` | Service-extension KPI definitions. |
| `selection`, `report` | viewer save | A saved selection; a trimmed report marker. |

Keys are set by patching the map (`RezDb::patch_recording_metadata`) so a
key another tool wrote survives.

## 8. Time

Row timestamps are **anchored**, not wall-clock: `timestamp = anchor +
monotonic elapsed`, where `anchor` is `recordings.clock_anchor_wall_ns`, the
wall clock read once at recording start. This keeps rows strictly increasing
through a wall-clock step. The wall clock at any row is
`timestamp + :wall_offset`; `clock_offsets` summarizes the same series at
seal boundaries for consumers that do not decode segments. Windows are stored
relative to the row (§5.2) so they survive the anchoring unchanged.

A parquet-ingested recording has no monotonic clock; its anchor is `0` and
its timestamps are the parquet's own. Such a recording has no windows.

## 9. Retention and eviction

The only destructive operation. `evict_before(cutoff)` deletes, in **one
transaction**, every segment whose `last_ts < cutoff` and every WAL row whose
`ts < cutoff`. Segment granularity is deliberate: a straddling segment stays
whole, so a buffer holds *at least* the lookback. The two deletes must land
together — removing a segment lowers the live-WAL watermark, and rows the
segment covered would otherwise come back to life as a tail.

**Known limit.** Eviction is by timestamp only and does not know which WAL
row anchors a span's metadata (§6.1) or schema (§6.2). A lookback shorter
than the seal policy's `max_age` (300 s default) can delete an anchor while
later rows of the span survive; those rows then materialize with no metadata
(sampler table) or are skipped (group table). Tracked in `docs/backlog.md`.

## 10. Copies

`combine`, `filter`, `annotate`, hindsight's dump and `recording snapshot`
all produce a new archive by copying catalog rows and segment BLOBs
**verbatim** (`crates/rez/src/rez_v3_rewrite.rs`). Segments are never
decoded, except by `filter --metrics`, which projects columns at the Arrow
level and re-encodes. A copy renumbers `id` and `seq`, preserves `uuid`,
`labels`, `metadata`, `complete`, the clock anchor and offsets, and
materializes the source's live WAL tail into a sealed segment so the copy
has no WAL of its own to lose.

`combine` refuses two recordings with equal `uuid` and, unless told
otherwise, two with identical `labels`.

### 10.1 Reopening for append

An archive can be reopened by a later writer (`RezArchive::open`) and a
recording in it resumed (`resume_recording`) as a **new writer session**.
Nothing about the rows changes shape; four things are guaranteed:

- Segment numbering continues from `MAX(seq) + 1` per table, and the
  clock-offset series keeps what it had.
- The session's clock anchor must be later than the recording's newest row
  (segments and WAL together), and every row the session commits must be
  later still. A wall clock that went backwards across a restart is refused
  at resume and per tick, never written.
- The session is recorded under `writer_sessions` with its own anchor and the
  timestamp it resumed after, and as a `writer_session` event at the anchor.
- `complete` is cleared at resume and set by the session's finalize.

Row timestamps in the new session are the new anchor plus the new process's
monotonic elapsed time, so `timestamp + :wall_offset` stays the wall clock;
the gap between sessions is real time during which nothing was recorded.

## 11. Compatibility

- **What bumps the conventions version (`SCHEMA_VERSION`, `user_version`).**
  Any change a v3 reader would misread silently: a WAL row field added,
  removed or reordered; a change to the window or histogram column
  encodings; a change to the live-WAL rule; a catalog column a reader must
  understand to be correct. A reader refuses a version above its own.
- **What does not.** A nullable column added to `recordings` that an old
  reader can ignore (`uuid` was added this way); a new reserved metadata key
  (`producer_epoch`, `writer_sessions` were added this way); a new event
  kind. Old copiers drop what they do not know, which degrades to
  "unknown", never to wrong.
- **Field metadata is open.** A reader must ignore keys it does not know and
  a writer may add any.
- **The tar container is frozen.** It is read, upgraded, and never written.

## 12. Vocabulary that is rezolus's, not the format's

The format knows recordings, tables, segments, windows, WAL rows, epochs and
labels. `sampler` as the name of the table unit, the `<sampler>/<group>` key
shape, `source`/`host` as auto-filled labels, and the `systeminfo`,
`descriptions` and `service_queries` keys are rezolus conventions layered on
it. A different producer may use any table keys without `/`, any labels, and
any metadata, and every tool above will read the result; it will merely have
nothing to say in the places those keys feed.
