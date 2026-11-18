# Labels, storage keys and internal labels

What a series' labels are, where they come from, which keys are not labels at
all, and the rule for labels that exist for the query engine rather than for
the person reading a chart. Audited 2026-09-22 against metriken-query 0.24.0,
metriken-exposition 0.20.0 and this tree; the line references are to those.
The "Where the rule is enforced" section describes the state after
metriken-query 0.25.0 and the consumer change that followed it. The rows for
`unit`, `overflow`, `overflow_bound` and `scale`, and rule 1, were checked
on 2026-11-12 against metriken-storage 0.13.0, metriken-query 0.50.0 and
metriken-recorder 0.15.0. The `exported_<key>`
paragraph and the selector rule for an absent key were checked on
2026-11-11 against metriken-storage 0.12.0, metriken-query 0.49.0 and
metriken-recorder 0.14.0.

## Three kinds of key

A parquet column's field metadata is a flat `key → value` map. Every reader
splits it into three kinds, and the split is the whole subject of this document.

| kind | examples | where it lives | what a reader does with it |
|---|---|---|---|
| **storage key** | `metric`, `metric_type`, `unit`, `grouping_power`, `max_value_power` | column field metadata | consumed or discarded on load; never a label |
| **label** | `id`, `cpu`, `name`, `op`, `sampler`, `comm`, `pid`, `device`, `mount` | column field metadata | part of the series' identity; shown, listed, aggregated |
| **internal label** | `__name__`, `__run__`, `__uid__` | `__name__` and `__run__` are minted by the query layer; `__uid__` is minted by the agent at slot assignment and travels with the slot's labels, on disk included | part of the series' identity and matchable in a selector; hidden from listings and legends |

Prometheus draws the same lines. Its data model says "label names beginning
with `__` MUST be reserved for internal Prometheus use", the metric name is
"internally represented as a label pair with a special label name
(`__name__`)", and TYPE, HELP and UNIT are metadata lines in the exposition
format, not labels. The mapping below is that convention, spelled out for this
tree.

## Storage keys: the name and the shape

The on-disk name of a series is the `metric` key. The query layer exposes it as
`__name__`. **Neither side should be renamed to match the other**: `metric` is
the schema every archive ever written carries and every loader reads
(`metriken-query/src/parquet.rs:1708-1725`, `memory.rs:128-146`), and
`__name__` is what PromQL expects. The translation already happens at load.

`metric_type`, `unit`, `grouping_power`, `max_value_power`, `overflow`,
`overflow_bound` and `scale` describe the column's shape, and the loaders
strip them so they never become labels (metriken-storage's `STORAGE_KEYS`). Type is
not a label in Prometheus either. There is no `__type__` and there should not
be one.

A Prometheus exporter can put a label with one of those names on a series.
metriken-recorder (0.13.0 on) stores it as `exported_<key>` (`exported_unit`,
`exported_scale`), adding `exported_` again while that name is taken, as
Prometheus does for a scraped label that clashes with a target label. Two
series that differ only by such a label therefore stay two series.

| key | written by | read by | treatment |
|---|---|---|---|
| `metric` | agent (`src/agent/exposition/http/snapshot.rs:416`), Prometheus converter (`src/recorder/prometheus.rs:64`), parquet ingest backfill (metriken-storage `src/rez/parquet_ingest.rs`) | every loader | becomes `__name__`; column name minus `:buckets` is the fallback |
| `metric_type` | metriken-storage `src/rez/rez.rs`, combine (`src/parquet_tools/combine.rs:1387`), metriken-exposition's writer | loaders skip it; `recording metadata` prints it | selects counter/gauge/histogram handling |
| `unit` | a metric's own static metadata (`unit = "..."` in a sampler's `stats.rs`); the exposition crate never writes it except on `timestamp`; metriken-recorder's Prometheus converter (0.15.0) on a series whose name ends in a unit token or whose metric has a `# UNIT` line: the exporter's unit, one of metriken-storage's `prometheus_units::UNITS` values (`seconds`, `milliseconds`, `bytes`, `kibibytes`, `bits`, `percent` (0 to 100), `ratio`, `celsius`, `hertz`, `cores`, ...), and none on a `_count` or `_bucket` | loaders skip it as a label; MCP strips it from key lists; metriken-query's `MetricsSource::metric_unit` reads it, the viewers' metric catalogs list it as `unit`, and a KPI chart over a bare selector and the metric browser's chart scale by it (`format.unit`) | the unit the values are in |
| `grouping_power`, `max_value_power` | agent (`snapshot.rs:948`, `:2651`), Prometheus converter (`prometheus.rs:364`) | the parquet loader removes them (`parquet.rs:1708`); **the live-agent loader does not** (`memory.rs:139`) | histogram bucket configuration |
| `overflow`, `overflow_bound` | metriken-recorder's Prometheus converter (0.11.0), on a histogram whose `+Inf` bucket is served: the name of the counter of observations above the highest finite bound, and that bound in the stored unit | metriken-storage's parquet reader, which links the counter to the histogram's rows; metriken-query's `histogram_quantile` counts it | histogram overflow |
| `scale` | metriken-recorder's Prometheus converter, on a histogram whose bounds it multiplied before rounding them to integers: fixed by a time unit (1e9 for `seconds`, 1e6 for `milliseconds`, 1e3 for `microseconds`), otherwise the smallest power of 10, up to 1e9, that makes every bound an integer, or 1e9 with the bounds rounded when none does (0.15.0); absent when the factor is 1 | metriken-query (0.50.0) divides a histogram function's output by it and refuses to pool rows of different scales | a stored bound is the observation times `scale`; divided by it, the histogram reads in its `unit` |
| `acq_group` | declared statically on ~290 metrics | **removed before emission** (`snapshot.rs:735`, `:2342`) | never reaches a column |
| `description` | nothing in this tree (one test fixture) | | would become a label if anything wrote it |

## Labels: what identifies a series

Everything in field metadata that is not a storage key is a label, and series
identity is equality over the whole label map (`metriken-query/src/labels.rs:3`,
used at `memory.rs:66`, `segmented.rs:241`, `parquet.rs:1549`). A selector
matches on a subset and compares an absent key as the empty string, as
Prometheus does: `l=""` and `l!="a"` select a series without `l`, and
`l!=""` does not (metriken-storage 0.12's `Labels::matches`).

| label | origin | notes |
|---|---|---|
| `sampler` | set on every metric by the agent at `snapshot.rs:425` | what `sum by (sampler)` groups on. The static `sampler = "..."` declared in ten `stats.rs` files is dead: `:425` overwrites it |
| `id` | group index (`snapshot.rs:822` and siblings) | the most-aggregated label in the dashboards: `by (id)` 44 times |
| identity labels: `comm`, `pid`, `tgid`, `cgroup`, `device`, `mount`, `vendor` | `SlotIdentity::assign` (metriken's `src/group/identity.rs`, re-exported by `src/agent/identity.rs`) | **mutable**: a slot's labels change when its occupant does. Today that change is written into column metadata, which is the churn #1224 §2 removes |
| hw_sensors labels: `sensor`, `name`, `device`, `source`, `chip`, `channel`, `label`, `scope`, `board_model`, `soc_compatible`, `derived` | `insert_metadata`, once per slot (`src/agent/samplers/hw_sensors/linux/mod.rs`) | fixed for the slot's life: a slot is keyed by `sensor` and never reassigned |
| `__uid__` | `SlotIdentity::assign`, one per assignment, from the assignment's generation and the producer epoch | the occupant. Two assignments with identical labels (a PID wrapped onto the same comm, a cgroup recreated at the same path) get different uids and are different series. Internal: hidden everywhere, matchable in a selector |
| static labels: `op`, `vendor`, `error`, `kind`, `direction`, `state`, `counter`, `reason`, `clock`, `pipe`, `level`, `frequency`, `sensor` (on `drive_temperature_sensor`) | declared per metric | immutable for the life of the metric |
| `source` | `source=external` on external metrics (`snapshot.rs:1031`); `combine` adds `source`, `node`, `instance` to every column (`combine.rs:988`) | the metric browser hides `source` (`metric_browser.js:87`) |
| `__run__` | injected by the segmented reader when a histogram's bucket config differs across segments (`segmented.rs:431-450`) | already a reserved internal label; matchable (`:623`), listed only on conflict (`:705`) |

Column names carry sidecars that are not metadata at all and are skipped by
name: `:wall_offset`, `:window_begin`, `:window_width`, `<m>:window_begin`,
`<m>:window_width`, `<m>:buckets`, and the generation suffix `<name>#N` a
relabeled slot gets (`rez.rs:25-34`, `:1608`; `parquet.rs:1684-1704`).

## Internal labels: the rule

An internal label is one the query engine needs for identity that a person
reading a chart does not. The rule, adopted from Prometheus:

1. **Its name begins with `__`.** One writer puts one into column
   metadata: the agent writes `__uid__` (`SlotIdentity::assign`). A
   recording whose Prometheus series are named `..._nanoseconds` or
   `..._bytes` can also carry `__prometheus_name__`, the name the exporter
   served the series under. The loaders do not check, so any other `__` key in
   column metadata is kept as a label too. The readers add `__name__`,
   `__run__` and `__recording__`, and a recording's identity layers can
   carry `__producer__`.
2. **It is part of series identity and matchable in a selector.**
   `foo{__uid__="..."}` works; a bare `foo` returns every occupant
   as its own series.
3. **It is hidden.** Label listings (`counter_labels` and friends,
   `label_values`, the metric catalog, `describe-metrics`), legends and series
   keys in the viewer, and MCP output omit it.
4. **Aggregation drops it.** `by (...)` already keeps only what it names;
   `without (...)` drops what it names plus every `__` label, as it already
   drops `__name__`. An aggregate is over a set of series, and which occupant
   each input was is meaningless for the sum.

Rule 4 has a consequence worth stating: a bare selector over a recording with
task churn returns more series than visible label sets, two of which may show
identical labels. That is the truthful shape. The alternative, one series
across a PID reuse, attributes one task's counter to another, which is the
defect the uid exists to remove.

### Where the rule is enforced

One predicate on each side, and every consumer goes through it:

| layer | predicate | used by |
|---|---|---|
| metriken-query (0.25.0) | `is_internal_label`, `is_storage_key`, `STORAGE_KEYS` | both loaders (`Labels::from_metadata`), `without`/`ignoring`, default binary-op matching, `__run__` |
| rezolus, Rust | the same, imported | metric catalog `label_keys` (both viewers), MCP `describe-metrics`, anomaly label help, correlation selectors, TUI legend |
| metriken-charts, frontend | `assets/charts/util/labels.js`: `isInternalLabel`, `isStorageKey`, `visibleLabels`, `firstVisibleLabelValue` | web legends (`data.js`, `viewer_core.js`), boxplot, compare, explorers, quantile labels (`compare_math.js`) |

Two listings deliberately keep internal labels: metriken-query's own
`counter_labels`/`gauge_labels`/`histogram_labels` return full label sets,
because they are the series' identity and `__run__` has to stay discoverable
when a histogram's configuration drifted; and the server's `/api/v1/labels`
returns `__name__`, as Prometheus's does.

Before this was one predicate, every consumer hid labels by literal name
(`__name__` in the frontend, three storage keys in MCP, nothing in the
catalog), and the frontend named a series by "the first label that is not
`__name__`". `BTreeMap` sorts `__` before every letter, so the first `__`
label a reader emitted would have become the legend text of every series
carrying it. metriken-charts' `tests/labels.test.mjs` and
`internal_labels_and_storage_keys_are_not_listed_as_filter_keys` pin the
consumers; metriken-query pins the engine.

## Known divergences

- **Live and recorded histograms differed** before metriken-query 0.25.0: the
  live-agent loader kept `grouping_power`/`max_value_power` as labels and the
  parquet loader stripped them. Both loaders now use one storage-key list.
- **Dead `sampler` declarations.** Ten `stats.rs` files declare a static
  `sampler` label the agent overwrites at `snapshot.rs:425`.
- **`{__name__="x"}` routes but cannot evaluate**: the selector parser accepts
  it (`promql/mod.rs:381`), the dispatcher requires a bare name
  (`streaming/dispatch.rs:605`).
- **`caller_rows` is not read.** A `.rez` recorded by a 5.x
  `record --stream -o out.rez` holds identity transitions there; 6.0 reads
  such a table with the labels each segment carries and logs a warning
  (metriken-storage's `reader.rs`, `relabel`). 6.0 writes none: a `.dendro`
  streams agents and takes identity from each group's schema, and a `.rez`
  is scraped.
  See `docs/journal/2026-09-22-internal-labels.md`.
