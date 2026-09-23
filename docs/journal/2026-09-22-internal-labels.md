# Internal labels: the `__` rule, and what the incarnation id is called

- **Opened:** 2026-09-22
- **Status:** **OPEN — steps 1 and 2 shipped.** Step 1 is metriken-query
  0.25.0 (iopsystems/metriken#151, released in #152); step 2 is the rezolus
  consumers switching to its predicates. The audit is in
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

- **Name:** `__incarnation__`, the singleton form `__name__` and `__run__` use.
  Value: the id the agent mints into `SlotEntry` when a slot is assigned,
  deterministic from producer epoch, slot and first-seen stamp, so two recorders
  of one agent agree without coordination.
- **Emitter:** the archive reader, from the index. It is never written into
  column metadata, so no archive ever carries it and the on-disk schema does
  not change for it.
- **The rule** (`docs/labels.md`, "Internal labels"): `__`-prefixed labels are
  identity and matchable, hidden from listings and legends, dropped by
  `without` alongside `__name__`. Enforced by one predicate on the label name,
  not by lists of literal names.
- **Shape:** a bare selector over a churning recording returns one series per
  incarnation, some with identical visible labels. Accepted as the truthful
  shape; aggregation collapses them.

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
3. **The reader** (#1224 §2, reader side): `SlotEntry` gains the id; the
   indexed group source in `crates/rez` replays `caller_rows` into per-slot
   label timelines and emits `__incarnation__`. Verified against today's
   `--stream` archives, which carry index and column identity together.
4. **Writer side, additive**: evict `caller_rows` with segments; resend a
   `Full` at the seal cadence.
5. **Cutover branch**: descriptors become bare slot ids, identity leaves column
   metadata, format version bumps.

Step 2 before step 3 is the ordering that matters: the reader must not be the
first thing to put a `__` label in front of a legend that will print it.

## Related

- #1224 (6.0 plan, §2 column identity), #1272 (the stream ingest whose
  archives are step 3's oracle).
- [The recorder consumes the replication stream](2026-09-22-recorder-stream-ingest.md).
- Prometheus data model: "Label names beginning with `__` MUST be reserved for
  internal Prometheus use."
