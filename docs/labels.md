# Labels, storage keys and internal labels

What a series' labels are, where they come from, which keys are not labels at
all, and the rule for labels that exist for the query engine rather than for
the person reading a chart. Audited 2026-09-22 against metriken-query 0.24.0,
metriken-exposition 0.20.0 and this tree; the line references are to those.

## Three kinds of key

A parquet column's field metadata is a flat `key → value` map. Every reader
splits it into three kinds, and the split is the whole subject of this document.

| kind | examples | where it lives | what a reader does with it |
|---|---|---|---|
| **storage key** | `metric`, `metric_type`, `unit`, `grouping_power`, `max_value_power` | column field metadata | consumed or discarded on load; never a label |
| **label** | `id`, `cpu`, `name`, `op`, `sampler`, `comm`, `pid`, `device`, `mount` | column field metadata | part of the series' identity; shown, listed, aggregated |
| **internal label** | `__name__`, `__run__`, (planned) `__incarnation__` | never on disk; minted by the query layer or the archive reader | part of the series' identity and matchable in a selector; hidden from listings and legends |

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

`metric_type`, `unit`, `grouping_power` and `max_value_power` describe the
column's shape, and the loaders strip them so they never become labels. Type is
not a label in Prometheus either. There is no `__type__` and there should not
be one.

| key | written by | read by | treatment |
|---|---|---|---|
| `metric` | agent (`src/agent/exposition/http/snapshot.rs:416`), Prometheus converter (`src/recorder/prometheus.rs:64`), parquet ingest backfill (`crates/rez/src/parquet_ingest.rs:175`) | every loader | becomes `__name__`; column name minus `:buckets` is the fallback |
| `metric_type` | `crates/rez/src/rez.rs:1586`, combine (`src/parquet_tools/combine.rs:1387`), metriken-exposition's writer | loaders skip it; `recording metadata` prints it | selects counter/gauge/histogram handling |
| `unit` | a metric's own static metadata only (`unit = "..."` in a sampler's `stats.rs`); the exposition crate never writes it except on `timestamp` | loaders skip it; MCP strips it from key lists | display hint |
| `grouping_power`, `max_value_power` | agent (`snapshot.rs:948`, `:2651`), Prometheus converter (`prometheus.rs:364`) | the parquet loader removes them (`parquet.rs:1708`); **the live-agent loader does not** (`memory.rs:139`) | histogram bucket configuration |
| `acq_group` | declared statically on ~290 metrics | **removed before emission** (`snapshot.rs:735`, `:2342`) | never reaches a column |
| `description` | nothing in this tree (one test fixture) | | would become a label if anything wrote it |

## Labels: what identifies a series

Everything in field metadata that is not a storage key is a label, and series
identity is equality over the whole label map (`metriken-query/src/labels.rs:3`,
used at `memory.rs:66`, `segmented.rs:241`, `parquet.rs:1549`). A selector
matches on a subset and fails closed on an absent key (`labels.rs:100`).

| label | origin | notes |
|---|---|---|
| `sampler` | set on every metric by the agent at `snapshot.rs:425` | what `sum by (sampler)` groups on. The static `sampler = "..."` declared in ten `stats.rs` files is dead: `:425` overwrites it |
| `id` | group index (`snapshot.rs:822` and siblings) | the most-aggregated label in the dashboards: `by (id)` 44 times |
| identity labels: `comm`, `pid`, `tgid`, `cgroup`, `device`, `mount`, `sensor`, `vendor`, hw_sensors' `board_model`/`chip`/`channel`/`label`/`scope` | `SlotIdentity::set` (`src/agent/identity.rs:249`) | **mutable**: a slot's labels change when its occupant does. Today that change is written into column metadata, which is the churn #1224 §2 removes |
| static labels: `op`, `vendor`, `error`, `kind`, `direction`, `state`, `counter`, `reason`, `clock`, `pipe`, `level`, `frequency` | declared per metric | immutable for the life of the metric |
| `source` | `source=external` on external metrics (`snapshot.rs:1031`); `combine` adds `source`, `node`, `instance` to every column (`combine.rs:988`) | the metric browser hides `source` (`metric_browser.js:87`) |
| `__run__` | injected by the segmented reader when a histogram's bucket config differs across segments (`segmented.rs:431-450`) | already a reserved internal label; matchable (`:623`), listed only on conflict (`:705`) |

Column names carry sidecars that are not metadata at all and are skipped by
name: `:wall_offset`, `:window_begin`, `:window_width`, `<m>:window_begin`,
`<m>:window_width`, `<m>:buckets`, and the generation suffix `<name>#N` a
relabeled slot gets (`rez.rs:25-34`, `:1608`; `parquet.rs:1684-1704`).

## Internal labels: the rule

An internal label is one the query engine needs for identity that a person
reading a chart does not. The rule, adopted from Prometheus:

1. **Its name begins with `__`.** Nothing on disk uses the prefix, so an archive
   can never collide with one.
2. **It is part of series identity and matchable in a selector.**
   `foo{__incarnation__="..."}` works; a bare `foo` returns every incarnation
   as its own series.
3. **It is hidden.** Label listings (`counter_labels` and friends,
   `label_values`, the metric catalog, `describe-metrics`), legends and series
   keys in the viewer, and MCP output omit it.
4. **Aggregation drops it.** `by (...)` already keeps only what it names;
   `without (...)` drops what it names plus every `__` label, as it already
   drops `__name__`. An aggregate is over a set of series, and the incarnation
   of one input is meaningless for the sum.

Rule 4 has a consequence worth stating: a bare selector over a recording with
task churn returns more series than visible label sets, two of which may show
identical labels. That is the truthful shape. The alternative, one series
across a PID reuse, attributes one task's counter to another, which is the
defect the incarnation exists to remove.

### Where the rule is enforced today, and where it is not

Nothing implements the prefix rule. Every consumer hides labels by literal
name:

| consumer | hides | where |
|---|---|---|
| metriken-query loaders | `metric`, `metric_type`, `unit` (+ histogram config on the parquet path only) | `parquet.rs:1721`, `memory.rs:139` |
| metriken-query `without`, binary-op matching | `__name__` only | `aggregate.rs:85`, `binary.rs:293` |
| metriken-query `histogram_labels` | `__run__` unless it conflicts | `segmented.rs:705` |
| viewer legends (web, TUI) | `__name__` only, then "first label" | `data.js:259`, `:873`, `:1290`; `viewer_core.js:126`; `tui/query.rs:103` |
| viewer boxplot / compare / explorers | `__name__`, `endpoint`, `source` / `__name__` / `__name__`, `id`, `metric`, `metric_type`, `unit` | `boxplot.js:343`, `compare.js:616`, `explorers.js:47` |
| metric catalog (both viewers) | nothing | `crates/dashboard/src/metric_catalog.rs:27` |
| MCP | `metric`, `unit`, `metric_type` (+ `__name__` in correlation) | `describe_metrics.rs:17`, `anomaly_detection/mod.rs:188`, `correlation.rs:819` |

Because the frontend picks "the first label" for a legend and `BTreeMap` sorts
`__` before every letter, a `__` label that reached the viewer today would
become the legend text of every series carrying it.

## Known divergences

- **Live and recorded histograms differ.** The live-agent loader keeps
  `grouping_power`/`max_value_power` as labels (`memory.rs:139`); the parquet
  loader strips them (`parquet.rs:1708`). The same recording has two extra
  labels per histogram series when viewed live.
- **Dead `sampler` declarations.** Ten `stats.rs` files declare a static
  `sampler` label the agent overwrites at `snapshot.rs:425`.
- **`{__name__="x"}` routes but cannot evaluate**: the selector parser accepts
  it (`promql/mod.rs:381`), the dispatcher requires a bare name
  (`streaming/dispatch.rs:605`).
- **`caller_rows` is write-only.** The recorder stores identity transitions
  (`rez_sqlite.rs:979`); no reader consumes them. See
  `docs/journal/2026-09-22-internal-labels.md` for the reader work that will.
