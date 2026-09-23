# Internal labels: the `__` rule, and what the incarnation id is called

- **Opened:** 2026-09-22
- **Status:** **OPEN — steps 1–5 shipped, step 6 (retention of the index) in
  review.** Step 1 is metriken-query 0.25.0 (iopsystems/metriken#151,
  released in #152); step 2 is the rezolus consumers switching to its
  predicates; step 4 is #1277 on metriken-core 0.3.2 / metriken 0.11.1; step
  5 is #1280 on metriken-query 0.26.0. The audit is in
  [`docs/labels.md`](../labels.md); this entry records the decision and the
  order of work.
- **Driver:** the reader-side half of #1224 §2 needs a per-occupant id so a
  slot reassigned to a different task with the same labels (PID wrap, a cgroup
  recreated at the same path) becomes two series rather than one. That id must
  be part of series identity without appearing on every chart, which is what
  Prometheus reserves the `__` prefix for. Before adopting it, the question was
  whether the name and type should move to `__name__`/`__type__` as well, and
  what the viewer and metriken-query do with such labels today.
- **Owner:** Brian Martin

## What the audit settled

Full detail in `docs/labels.md`. The parts that decide things:

- `__name__` already exists in the query layer: metriken-query strips it from
  selectors (`promql/mod.rs:381`), injects it into results
  (`streaming/mod.rs:192`) and exempts it from `without` (`aggregate.rs:85`).
  The on-disk key is `metric` and the loader translates. **No rename**: the
  storage schema every archive carries stays `metric`; PromQL keeps `__name__`.
- Type is a storage key (`metric_type`) that loaders discard, exactly as
  Prometheus keeps TYPE out of the label set. **No `__type__`.** Same for
  `unit`, `grouping_power`, `max_value_power`.
- A reserved `__` label already exists: `__run__`, injected by the segmented
  reader on a histogram config change (`segmented.rs:431`), matchable, listed
  only on conflict, protected by a `debug_assert` rather than a rule. The
  convention is half-adopted, so adopting it fully is a codification.
- Every consumer hides labels by literal name, and the frontend's legend is
  "the first label that is not `__name__`". `BTreeMap` order puts `__` before
  every letter, so a new `__` label would become every legend today. That is
  the concrete thing the rule has to fix before the reader can emit one.
- Two loaders disagree: the live-agent path keeps `grouping_power` and
  `max_value_power` as labels, the parquet path strips them. A recording has
  two extra labels per histogram when viewed live.

## Decisions

- **Name:** `__uid__`, the singleton form `__name__` and `__run__` use, and
  Kubernetes's precedent for exactly this: an object recreated under the same
  name gets a new `metadata.uid`. `__incarnation__` was the first choice; it is
  real distributed-systems vocabulary (SWIM, Cassandra gossip) but reads as
  mystical outside it.
- **Where it is minted, revised.** The first plan minted it in `SlotEntry`
  and had the reader emit it from the index, never on disk. Building step 3
  showed that is the wrong place: `SlotIndex::observe` emits an entry only
  when a slot's *labels* change, so a PID-reuse reassignment with identical
  labels — the case the uid exists for — produced no entry and would have got
  no uid. The assignment event is `SlotIdentity::set`, which already takes a
  generation, so the uid is minted there, once per assignment from the
  generation and the producer epoch, and inserted into the slot's label map.
  It then travels with the labels everywhere they go without any consumer
  being taught about it: snapshot descriptors, `.rez` column metadata, the
  index entries a stream carries. Because step 2 landed first, every listing
  and legend already hides it. The Prometheus exporter drops it, since
  Prometheus strips `__` labels after relabeling anyway.
- **What that buys today, before the cutover.** The `.rez` writer opens a new
  column when a descriptor's metadata changes, so a same-label reassignment
  now becomes a second column with its own `__uid__`, and metriken-query keys
  series on the full label set. The PID-reuse artifact is fixed in the current
  format, with no reader change. The cost is a schema resend on a same-label
  reassignment that was previously silent, which is the correct behaviour and
  leaves with the rest of identity at the cutover.
- **The rule** (`docs/labels.md`, "Internal labels"): `__`-prefixed labels are
  identity and matchable, hidden from listings and legends, dropped by
  `without` alongside `__name__`. Enforced by one predicate on the label name,
  not by lists of literal names.
- **Shape:** a bare selector over a churning recording returns one series per
  incarnation, some with identical visible labels. Accepted as the truthful
  shape; aggregation collapses them.

## Measured: what the uid costs the scrape path

`delta`, 32 cores, main without the uid commit against the uid build, same
sampler set, both scraped concurrently at 1 s for 120 s; `churn.sh` runs
short-lived tasks that accrue CPU so slots are reassigned during the window.

| run | without | with | ratio | per scrape | body |
|---|---|---|---|---|---|
| churn | 0.76 s | 0.85 s | 1.12x | 6.33 → 7.08 ms | 512 → 574 KB |
| idle | 0.66 s | 0.71 s | 1.08x | 5.50 → 5.92 ms | 524 → 587 KB |

A first comparison against an older control (`alpha.26`, before `hw_sensors`)
gave the same shape and was confounded by the sampler set; these are not.

The profile (perf on both agents, demangled with `rustfilt`) shows no new hot
function. The growth is msgpack encoding — `write_str`, `Marker::to_u8`, the
compound serializer — and `SlotIdentity::set` at 1.2% under churn. The cost
is proportional to the bytes added: `/metrics/binary` re-encodes every
descriptor's metadata on every scrape, and `__uid__=<16 hex>` on ~2,000
slotted descriptors is ~60 KB per scrape. The stream sends a schema once per
generation and does not pay it per tick; the cutover removes it entirely.

Two things the profile found that were not the question asked:

- **A re-announced slot must keep its uid.** The first cut minted one per
  `set`. `drivehealth` sets every drive's labels on every sweep and `ethtool`
  every interface's on every refresh, so every such series would have split
  at every refresh, and the network group's schema was rebuilt and hashed
  every tick for an occupant that never changed. Fixed: a live slot setting
  identical labels keeps its uid and publishes nothing; a new uid is minted
  only for a slot that was cleared (the PID-reuse case) or whose labels
  changed.
- **The hashing on the hot path predates the uid.** `identity_fold_metadata`
  is 13–19% of the agent's CPU in both builds: the skeleton cache byte-hashes
  every slot's labels every tick to notice a metadata change, and
  `GroupSchema::hash` re-serializes and hashes a whole schema on any miss.
  Every identity write now goes through `SlotIdentity::set`/`clear` except
  one startup write in `hw_sensors`, so a per-group generation bumped there
  is an O(1) change signal that could replace the per-tick fold. That is a
  separate change and a larger saving than the uid costs.

## Measured: the version fold

metriken's group metadata store now carries a version bumped on every
mutation (iopsystems/metriken#153), and the skeleton cache folds that one
value per metric instead of hashing every entry's labels. Same harness, main
with the uid against this build:

| run | before | after | ratio | per scrape |
|---|---|---|---|---|
| churn | 0.92 s | 0.78 s | 0.85x | 7.67 → 6.50 ms |
| idle | 0.81 s | 0.68 s | 0.84x | 6.75 → 5.67 ms |

Bodies are byte-identical, as they should be: nothing on the wire changed.
The profile shows `identity_fold_metadata` at 13.6% before and absent after;
what remains at the top is msgpack encoding of the schema every scrape,
which is the snapshot endpoint's known cost and the cutover's to remove.
Against main *without* the uid the net is about 0.94x, so the correctness
fix and the hashing removal together leave the scrape path cheaper than
where the day started.

The signal lives in metriken's store rather than in `SlotIdentity` because
one sampler (`hw_sensors`) writes metadata directly, and the pinned test
`declared_group_schema_reflects_metadata_mutated_at_a_stable_index` mutates
through metriken too; both still force a rebuild.

## Path forward, in order

1. **metriken-query** (in-house, `~/workspace/iopsystems/metriken`): a single
   `is_internal(name)` predicate; use it in `without` and binary-op matching
   where only `__name__` is exempted today, in the `*_labels()` listings and
   `label_values`, and make `__run__` follow it instead of its own checks. Unify
   the two loaders' storage-key skip lists into one so live and recorded
   histograms carry the same labels. A test pins both the storage-key list
   and the predicate, so a key added to one loader and not the other fails a
   build rather than surfacing as two extra labels on a live histogram.
   Release, bump here.
2. **rezolus consumers**: replace the literal `__name__` checks in the web and
   TUI legends, boxplot, compare and explorers with the prefix predicate in one
   helper; the metric catalog and MCP listings use the same predicate. Small,
   mechanical, and it can land before anything emits a `__` label.
3. **The uid at assignment** (revised, see Decisions): `SlotIdentity::set`
   mints `__uid__` into the slot's labels; the exporter drops it.
4. **Replace the per-tick identity fold with a version** — done, see
   "Measured: the version fold". Shipped on metriken-core 0.3.2 / metriken
   0.11.1 (a defaulted trait method, so a patch bump rather than the 0.4.0 /
   0.12.0 first planned).
5. **The reader** (#1224 §2, reader side) — done (#1280), see "The reader"
   below.
   `crates/rez/src/indexed.rs` replays `caller_rows` into per-slot occupancy
   spans and splits a group table's columns by occupant into a
   `MemoryStore`; the archive reader takes that path for every table whose
   stream has index entries. Verified against today's `--stream` archives,
   which carry index and column identity together. Needs metriken-query
   0.26.0 (iopsystems/metriken#155).
6. **Retention of the index** — done, see "Retention of the index" below.
   The subscriber restates every stream's slot set as `Full` entries every
   seal age; the writer cuts a stream's `caller_rows` back to the latest
   `Full` at or before the row cutoff; the reader starts its replay at the
   last `Full` before a table's first row.
7. **Cutover branch**: descriptors become bare slot ids, identity leaves column
   metadata, format version bumps.

Step 2 before step 3 was the ordering that mattered: the uid must not be the
first `__` label in front of a legend that would print it.

## The reader

A group table's column is a slot, and the archive reader used to file every
row of a column under the labels in that column's field metadata. When a
slot changes hands inside a segment the table builder already opens a second
column (`{metric_id}x{slot}#2`) with the new labels, so the parquet path does
see a handover — but only because the writer copied the labels into the
column, which is what step 7 stops doing.

`crates/rez/src/indexed.rs` reads the table the other way round:

- **Replay.** The stream's `caller_rows` entries, in `(ts, seq)` order, go
  through the same `SlotIndex::apply` the subscriber uses. After each entry
  the live set is diffed against the open spans: a slot whose labels changed
  or that left closes its span at the entry's timestamp and a new one opens
  there. An entry takes effect *at* its timestamp because the recorder
  commits a tick's rows and the entries describing them together, stamped
  alike. A `Delta` before the first `Full` is skipped and counted, not fatal.
- **Split.** Every segment (sealed, then the WAL tail) is decoded eagerly with
  `read_table_parquet`, which was a test-only function until now. Each
  column's slot is its `id` metadata, or failing that the `{metric_id}x{slot}`
  name with any `#generation` suffix removed. Each row goes to the series of
  the slot's occupant at that timestamp: the column's own labels (storage
  keys removed, as the parquet loader does) with the occupant's index labels
  laid over them, index winning. A row with no occupant on record keeps the
  column's labels and is counted; nothing is dropped.
- **Compose.** The series go into a `MemoryStore` — counters and gauges with
  their per-sample windows from the table-level window pair, histograms as
  cumulative sparse snapshots with `__run__` numbered the way the segmented
  reader numbers them — and the store declares the table's row timestamps as
  its own. It composes as a union child and as a composition source beside
  the parquet-backed tables, so nothing above `TableReader` changed.

The oracle: the same six-tick archive with a handover is written twice, with
and without its index entries, and `rate()` over it — labels, values,
timestamps, uncertainty bands — is identical on both paths. On the
index-only shape the cutover will write, the parquet path sees one series
per slot and the indexed path sees three, split at the handover, with the
second occupant's own slope. A mutation that disables the index path fails
the split tests and leaves the oracle green, which is the intended shape: the
oracle proves agreement, the split tests prove the path is taken.

Not measured yet: open cost. The indexed path decodes every segment of a
table at first query rather than reading footers, which for a 300-slot task
table over a long recording is the whole table in memory. The parquet path
was tuned for the opposite (footer-only probes, ~1.37 ms per segment). The
tables this applies to are exactly the churning ones, so this needs a number
before step 7 makes it the only path.

## Retention of the index

Two problems with the index as #1272 wrote it. A rolling buffer evicts rows
by age but never touched `caller_rows`, so a hindsight buffer fed by the
stream would keep every identity change since connect for the life of the
process. And a reader of a long recording replayed the stream's whole
history to attribute its last hour, because the only `Full` was the one the
producer sent at connect.

The plan said "resend a `Full` at the seal cadence", by the producer. It is
the subscriber that does it. The subscriber holds the same slot set the
producer does — rule 10 checks that hash on every rows frame — so it can
write the restatement itself, from `SourceIndex::full_entries`, stamped with
the rows that tripped it, and the producer never has to know the recorder's
cadence. `recorder::stream::RESTATE_EVERY` is 300 s, the seal policy's
`max_age`, so any segment's rows are at most one restatement from a `Full`.
Rows skipped under rule 10 do not trip it: the set the subscriber holds is
not the one those rows named.

Which entries are `Full` is not stored. The catalog keeps the blob opaque,
and the two consumers of the fact get it two ways:

- **The writer** sees each entry's kind on the way in (`IndexRow::full`,
  in memory only) and keeps the `Full` timestamps per (recording, stream).
  At retention it cuts each stream's history at the latest `Full` not after
  the row cutoff — strictly before it, so the `Full` itself stays — and a
  stream with no `Full` on record keeps everything. A writer that starts
  against an archive with existing entries therefore evicts nothing until
  its first restatement lands, which errs on keeping. Entries after the cut
  are kept however old the rows they describe are; the safe side.
- **The reader** walks a stream's entries newest-first from the table's
  first row and stops at the first `Full` it decodes
  (`RezDb::last_caller_row_at_or_before` with a predicate the reader
  supplies), then reads forward from there. One restatement period of
  entries at most, not the history.

Pinned: a subscriber restates at the interval and not before, flagged and
stamped as the rows; retention keeps the entry at the cut and the other
stream's history; a recording whose head and index were evicted back to
the restatement still attributes the surviving rows.

## Related

- #1224 (6.0 plan, §2 column identity), #1272 (the stream ingest whose
  archives are step 3's oracle).
- [The recorder consumes the replication stream](2026-09-22-recorder-stream-ingest.md).
- Prometheus data model: "Label names beginning with `__` MUST be reserved for
  internal Prometheus use."
