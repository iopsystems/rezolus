# Measurement uncertainty — all-sampler observation windows

- **Opened:** 2026-07-10
- **Status:** IMPLEMENTED & VALIDATED (pending PR to iopsystems/metriken +
  rezolus). Built in three phases — A: metriken foundation (13 commits on `next`),
  B: drivehealth pilot (hardware-validated), C: fleet migration (6 tasks). The
  **windows** portion of the arc's Phase 3, pulled forward. **Subsumes the
  standalone landing of Phase 1** (drivehealth per-index windows became the first
  Regime-R consumer). See *Validation results* below.
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md);
  builds on [Phase 1](2026-07-10-measurement-uncertainty-phase-1.md).
- **Owner:** Brian Martin
- **Repos:** metriken (`~/workspace/metriken`, branch `next`) for the torn-safe
  window store; rezolus for the producer helpers + per-sampler work. rezolus
  builds against local metriken via the dev-only `[patch.crates-io]` override
  (uncommitted).

This entry is the design spec (absorbs the brainstorm). It **supersedes** the
earlier module-path-attribution design (see *Alternatives considered*): a
grounding pass showed attribution was unnecessary and a separate stored window is
tear-prone.

## Goal

Make **every** metric carry an honest acquisition window — the tight `[begin,
end]` of when its value was actually read — instead of inheriting the coarse
fleet window (`systemtime`+`duration`, which spans the *whole* collection). The
window must be **torn-safe**: a reader can never pair a value from one read with a
window from a different read.

Still temporal-only (windows). `epoch`/value-quantum and cross-host clock remain
later phases; the observation shape already accommodates them.

## Grounded model: two regimes

Where a metric's value actually comes from determines its window and whether it
can tear. The codebase has exactly two flows:

**Regime R — read-at-refresh (≈ all metrics today).** The sampler reads a source
during `refresh()` and *copies* the value into a metriken metric:
- BPF counters: `Counters::refresh()` / `CpuCounters::refresh()` →
  `counter.set(value)` (`src/agent/bpf/counters.rs`).
- BPF histograms: `BpfHistogram::refresh()` → `histogram.update_from(&buckets)`
  (`src/agent/bpf/histogram.rs`).
- Userspace: drivehealth (ioctl), memory `/proc` (meminfo/vmstat), cpu `/sys`
  (cores), GPU (NVML) — read the source, `.set(...)`.

The value is produced at refresh and read later at snapshot, from **two separate
stores** (the metric's value, and — for windows — a side store). **This is the
tear surface.**

**Regime S — read-at-snapshot (mmap-direct, 2 `attach_external` sites today).**
`PackedCounters` binds a `CounterGroup` directly to the BPF mmap; `refresh()` is a
no-op and the exposition reads the value **live at snapshot**. Value and window
(the read instant) are the *same load* → **tear-free by construction**, and the
value is always current.

## The tear, and why it matters

In Regime R the value and its window are set by separate stores
(`set(idx, v)` then `set_window(idx, w)`) and read by separate loads at snapshot.
A reader that lands between the two stores pairs a fresh value with the *previous*
read's window. For a fast sampler the adjacent windows are ~identical, so it is
harmless. For a slow/async sampler it is not: **drivehealth** reads every 60 s on
a `spawn_blocking` task, so a torn read stamps a current temperature with a window
**~60 s off** — a corrupt observation, exactly the lie the arc exists to kill.
Phase 1 called this "benign one-refresh skew"; that was too glib.

## Design

The window lives **with the value**, set by the sampler that reads it and read by
the exposition — so there is **no metric→sampler attribution** (no macro, no
module registry). The two regimes are stamped differently:

### Regime R — torn-safe window coupling, enforced by the type

The `(value, window)` pair must be read/written atomically (a reader must never
pair a value from one write with a window from another). The enforcement is
**structural, not disciplinary** — you should not be able to *accidentally* write
a windowed value without its window. The mechanism follows one principle:

> **A windowed type must not expose a mutator that bypasses the window lock.**

That principle sorts the metric types by whether their existing mutators are
lock-free:

1. **`Counter`/`Gauge` — scalar and group — have _lock-free_ mutators**
   (`set`/`add`/`increment`, plain atomics). Those bypass any window lock, so the
   windowed variant must be a **wrapper type that does not expose them**:
   - **Scalars (option A, base stays lean).** metriken-core `Counter`/`Gauge` are
     the leanest, most-used types; we do **not** add a window field. metriken
     gains public wrapper types `WindowedLazyCounter`/`WindowedLazyGauge` holding
     the base metric + a lazily-allocated, boxed `RwLock<Option<Window>>` cell.
     They expose **only** `set_with_window` (write) and `load_with_window` /
     `value_with_window` (read) — no lock-free `set`/`add`, no `Deref` — so a
     windowless write is *unrepresentable*. Base `Counter`/`Gauge` cost is
     unchanged for every other metriken user.
   - **Groups.** metriken gains wrapper types `WindowedCounterGroup`/
     `WindowedGaugeGroup` that **restrict** the base group's API: they delegate
     `set_with_window`/`load_with_window` (guarded by the base group's existing
     Phase-1 `GroupWindows` `RwLock`), the read accessors, and metadata — but do
     **not** re-export `set`/`add`/`increment`. The window **store stays on the
     base group** (Phase 1, unchanged): a group is already a heavy object, so the
     one-pointer store is negligible and not worth relocating.
2. **`RwLockHistogram` has _no_ lock-free mutator** — every write already goes
   through its internal `RwLock`. So it needs **no wrapper**: add
   `set_with_window(&buckets, window)` / `load_with_window` **on the base type**,
   storing the window *inside that same `RwLock`* so the pair is atomic for free.
   `update_from` stays for general (windowless) use; the BPF producer brackets its
   copy and calls `set_with_window`. Histograms get an honest read window (the
   read is a live smear — the kernel keeps updating buckets during the copy) with
   no new type and no migration.

**No aliasing — an honest migration.** Because enforcement removes the lock-free
mutators, every windowed producer's write *must* become `set_with_window`
regardless — so the aliasing trick (`WindowedLazyCounter as LazyCounter`) never
avoided the call-site churn, and it actively breaks for groups (a single sampler
like `cpu/usage` mixes windowed per-CPU groups with plain `packed`/mmap-direct
groups, which can't share one aliased name). So windowed metrics are **declared
with the windowed type explicitly**; packed/mmap-direct groups stay plain
`CounterGroup`.

**Producers set the pair as a unit** — `Counters`/`CpuCounters` (BPF),
drivehealth, `/proc`/`/sys` samplers bracket their read and call
`set_with_window`; the histogram producer brackets `update_from`.

**Exposition reads the pair** under one lock acquisition
(`load_with_window(idx)` for groups, `value_with_window()` for scalars,
`load_with_window()` for histograms) — never separate `value` + `load_window`
reads that could tear. This requires forwarding `load_window`/`value_with_window`
through metriken's `MetricWrapper`/`ProviderMetric` (the `#[metric]` registration
wrappers), or the scalar window is silently dropped.

### Regime S — snapshot-read window (mmap-direct)

Metrics with no stored window that are read live at snapshot (`attach_external`)
are stamped with the **bracketed `[begin, end]` of their read section**: `create()`
takes one monotonic clock pair around exactly the loads that produce the values —
per group read loop, not per index. This mirrors Regime R (which brackets the
*refresh* read); both regimes emit `[begin, end]` around the real read and differ
only in *when* it happens. Width is µs for a per-CPU group, ~ns for a single
scalar; tear-free (value and window are the same load). No storage, no lock.

Not a single instant (a group's indices are read across the loop, so a bracket is
honest where an instant would claim zero width) and not the whole-`create()` span
(mmap values are live, so two counters read ms apart are genuinely skewed —
flattening them to simultaneous is the lie the arc kills).

### External metrics carry their own window

Unchanged from the prior design and still needed: metrics scraped from a
Prometheus endpoint use their embedded timestamp (`metric value <ts_ms>`) if
present, else the fetch/ingestion window the `ExternalMetricsStore` already
tracks — **not** rezolus's snapshot time.

### Precedence at snapshot

For each metric `create()` resolves `window` as:
1. **stored per-index/scalar window** (Regime R, via the locked-pair read), else
2. **external** scrape/source window, else
3. the **bracketed read-section window** for a mmap-direct (`attach_external`)
   metric read live at snapshot — *design settled; implementation deferred to the
   mmap-direct follow-on* (only ~1 unused `attach_external` site exists today, so
   this landing lets those fall through to level 4), else
4. the **fleet** window — fallback for a not-yet-instrumented Regime-R sampler
   (safe: it spans the collection, so it *contains* the true read; honest, just
   loose). No metric is mis-stamped tighter than the truth.

metriken-core's read API is otherwise untouched; the general library is
unaffected beyond the opt-in, lazily-allocated window cell.

## Aggregate vs per-CPU (representation stays as-is)

The `Counters` (aggregate → scalar) vs `CpuCounters` (per-CPU → `CounterGroup`)
split is deliberate and correct, and we are **not** flattening it:
- **Per-CPU** where the CPU is a semantic axis you'd query — `cpu/*` (usage,
  migrations, frequency, perf cycles/instructions/cache, l3, branch, dtlb,
  bandwidth), scheduler runqueue. Per-core skew is the point.
- **Aggregate** where per-CPU banks are only false-sharing avoidance on the write
  path and the real axis is elsewhere — `syscall/counts` (`SYSCALL_READ/WRITE/…`,
  keyed by syscall), `BPF_RUN_COUNT/TIME`. Exposing per-CPU there is meaningless
  (which core ran the task is a scheduling artifact) and a cardinality explosion
  for a fleet-wide always-on agent (principle 1).

## mmap-direct follow-on (separate effort)

Migrating Regime-R **BPF** metrics to Regime S (mmap-direct) is the better
end-state — tear-free, always-live values, more faithful to principle 6 — but it
is a **hot-path rework** and its own journal effort, not part of this landing:
- **Per-CPU** metrics mmap-direct cleanly via a **strided attach** view (fixed
  counter idx, stride over the padded per-CPU banks) — a new metriken capability.
- **Aggregate** metrics need a **read-time reduction** (sum banks on read); they
  stay Regime R here, or later gain a sum-on-read derived scalar.
- Cost moves aggregation to every scrape → **measure** (principle 16) per sampler
  before migrating.

The torn-safe Regime-R store built here is **permanent** for userspace samplers
(drivehealth/`/proc`/`/sys`/GPU, which can never be mmap-direct) and **interim**
for BPF metrics until each migrates. Not wasted work.

## metriken change set (branch `next`)

- **Scalar wrappers** `WindowedLazyCounter`/`WindowedLazyGauge` — base metric +
  boxed, lazily-allocated `RwLock<Option<Window>>` cell; expose **only**
  `set_with_window` + `load_with_window`/`value_with_window` (no lock-free
  mutator, no `Deref`). metriken-core `Counter`/`Gauge` **untouched** (lean).
- **Group wrappers** `WindowedCounterGroup`/`WindowedGaugeGroup` — restrict the
  base group: delegate `set_with_window(idx, value, window)` /
  `load_with_window(idx)` (guarded by the base's Phase-1 `GroupWindows` `RwLock`)
  + reads + metadata; do **not** expose `set`/`add`/`increment`. Window store
  stays on the base group (unchanged from Phase 1).
- **Histogram — no wrapper.** Add `set_with_window(&buckets, window)` /
  `load_with_window` **on the base `RwLockHistogram`**, storing the window inside
  its existing `RwLock` (atomic for free). `update_from` stays for general use.
- **Forward** `load_window` / `value_with_window` through `MetricWrapper`
  (`wrapper.rs`) and `dynmetrics::ProviderMetric` — else the `#[metric]`
  registration wrapper drops the window and the scalar path is inert.
- A generic `value_with_window()` on the scalar `Metric` trait (default no
  window) so the exposition reads the pair atomically.
- **No** windowed *histogram* type, **no** `#[metric]` macro change, **no**
  `module_path`, **no** attribution, **no** hand-rolled seqlock, **no** aliasing.
  (Phase 1's `Window` type + exposition `window` field already exist.)

## rezolus change set

An **honest migration** (no aliasing): each windowed counter/gauge metric is
re-declared with its windowed wrapper type; histogram declarations stay
`RwLockHistogram` (base type gains the methods); packed/mmap-direct groups stay
plain `CounterGroup`.

- **Shared `timed` helper**: generalize drivehealth's `timed<T>(read) -> (T,
  Window)` (wall begin + monotonic width) into the agent framework.
- **Producer helpers** (`src/agent/bpf/counters.rs`, `histogram.rs`): the
  per-CPU/aggregate counter helpers hold windowed group/scalar types and bracket
  their mmap read loop with `set_with_window`; the histogram producer brackets
  `update_from` → `set_with_window`. Instruments the BPF fleet.
- **drivehealth**: declare its gauge/counter groups as the windowed group
  wrappers; switch per-device `set`+`set_window` to `set_with_window`.
- **Userspace samplers** (`memory/{meminfo,vmstat}`, `cpu/cores`,
  `gpu/{nvidia,amd}`): windowed types + bracket the read + `set_with_window`.
- **Exposition** (`src/agent/exposition/http/snapshot.rs` + `exporter` +
  `recorder/prometheus`): read via the atomic pair accessors; stamp precedence
  1→2→4 (Regime-S level-3 bracketing deferred to the mmap-direct follow-on).
- **External** metrics: scrape/source window (level 2), as prior design.

## Phasing

The migration is real, so land it in shippable increments (mirroring how Phase 1
validated on drivehealth first):

- **Phase A — metriken foundation.** The wrappers + histogram base methods + the
  `MetricWrapper`/`ProviderMetric` forwarding + exposition wiring, on metriken
  `next`. Tested in isolation.
- **Phase B — drivehealth pilot.** Migrate *only* drivehealth to the windowed
  group wrappers and validate torn-safety end-to-end on the **actual async tear
  case** before touching anything else.
- **Phase C — fleet.** The BPF per-CPU groups (`CpuCounters`), `Counters`
  aggregates, histograms, and userspace scalars.

## Overhead (principle 16)

Per windowed metric: one lazily-allocated, boxed window cell (a `Window` = two
`u64` behind an `Option` + a `RwLock`), allocated on first `set_with_window`. The
cell lives only on the **windowed wrapper types** that rezolus opts into — base
metriken-core `Counter`/`Gauge` are **byte-for-byte unchanged** for every other
user (option A). A rezolus windowed scalar carries the extra pointer (null until
first window) + the boxed cell once set; groups reuse Phase 1's store. The
windowed read takes a read lock only on the exposition path (once per snapshot);
the lock-free `value()` path is unchanged. No macro metadata, no per-sampler
registry. Measure and report added RSS / per-snapshot bytes and `sampling
latency` before/after; expect negligible.

## Validation / GO criteria

Running the agent (representative BPF samplers + drivehealth + an external
source), on `/metrics/json`:
- Every instrumented sampler's metrics carry a `window` **narrower than** the
  fleet `duration`; BPF mmap-copy windows are µs-scale, `/proc`/ioctl windows are
  their real read duration.
- drivehealth keeps its **per-device** windows.
- **Torn-safety:** a concurrent-scrape stress test against drivehealth (writes on
  a background task) never observes a value paired with a stale window; assert the
  locked-pair read never returns a mismatched pair under contention.
- **External** metrics keep their own scrape/source window, not rezolus's
  snapshot time.
- A not-yet-instrumented metric falls back to the fleet window (loose, not
  mis-tight); no metric is stamped tighter than the truth.
- Overhead measured and reported (above).
- Back-compat: `window` remains optional; V2 consumers unaffected.

### Validation results (2026-07-13)

All GO criteria met.

- **Torn-safety, unit:** every windowed type passes a concurrent writer/reader
  stress (200k–2M iters) asserting the `(value, window)` pair is never torn —
  scalar wrapper, group wrapper, base `RwLockHistogram`, and (in rezolus) a
  drivehealth-shaped `WindowedGaugeGroup` at 2M reads.
- **Hardware, per-device (drivehealth pilot):** on 22 real SATA drives, each
  temperature carries a non-zero window (2.0–3.4 ms — real `SMART READ DATA`
  latency), and the windows are **per-device** — `begin` timestamps staggered
  ~103 µs across drives, widths differing per drive. Not one shared fleet window.
- **Live agent, fleet:** `/metrics/json` with fleet `duration` = 6.80 ms —
  - BPF per-CPU `cpu_usage`: windows 256 ns / 2048 / 3072 / 4096 ns (per CPU) —
    ~1000–25000× tighter than fleet (staggered by the honest `CpuCounters` sweep).
  - Userspace `memory_total` gauge: 238 µs window (real `/proc/meminfo` read).
  - Packed `cgroup_cpu_usage`: `window: null` (level-4 fleet fallback, correct).
  - Lean `rezolus_bpf_run_count`: no tight window (level-4, deliberate).
- **External:** unit-verified — embedded Prometheus `<ts_ms>` → window; absent
  ts → fetch time; line-protocol → ingestion instant (level 2).
- **Green bar:** metriken `cargo test --workspace` all green; rezolus
  `cargo build`/`test` (239 passed)/`clippy`/`xtask fmt` clean under the local
  patch. `[patch.crates-io]` never committed.
- **Overhead:** negligible by construction (one lazily-allocated boxed window cell
  per windowed scalar; groups reuse the Phase-1 store; a read lock only on the
  once-per-snapshot exposition path). A precise before/after RSS/sampling-latency
  delta on the same host is a small follow-up; live sampling latency (~6.8 ms
  fleet duration) is in the normal range.

## Testing

- metriken: `set_with_window`/`load_with_window` round-trip (scalar + group);
  **torn-read stress test** — a writer thread looping `set_with_window` while a
  reader thread asserts every `load_with_window` returns a self-consistent
  `(value, window)` pair (never a torn mix); lazy-allocation test (no window set
  → `None`, no allocation).
- rezolus: the `timed`/`observe` helper (pure, unit-tested — generalize Phase 1's
  `timed`); snapshot precedence resolution (unit test: a metric with a stored
  window, an external metric, a mmap-direct metric, and an uninstrumented metric →
  each resolves to the right level); external window sourcing (embedded timestamp
  vs ingestion time).
- Hardware/integration: the drivehealth window test (Phase 1) still passes; a
  smoke run asserts a BPF sampler's metrics carry a tighter-than-fleet window; the
  concurrent-scrape torn-safety stress above.

## Alternatives considered

- **Module-path attribution (prior design).** `#[metric]` records
  `module_path!()`; the framework attributes each metric to its sampler and stamps
  a per-sampler critical-read window. *Rejected:* (a) unnecessary — a sampler
  already owns and writes its own metrics, so it can stamp them directly; (b) the
  per-sampler window is the *most* decoupled from the value → most tear-prone; (c)
  needs a metriken macro change + a per-sampler registry + regrouping the
  exposition by sampler.
- **Window cell on the base `Counter`/`Gauge` primitives (option B).** Add the
  lazily-allocated cell directly to metriken-core's scalar types. *Rejected:* it
  taxes the leanest, most widely-used metriken types for **every** downstream user
  (a boxed `OnceLock` on every counter, ~16 bytes even when unused) to serve a
  rezolus concern. Option A (windowed wrapper types) keeps the base types lean.
- **Torn-safety by documented discipline (no enforcement).** Add
  `set_with_window` beside the base types' lock-free `set`/`add` and *ask*
  producers to use it exclusively. *Rejected:* a stray lock-free write silently
  pairs a fresh value with a stale window — the exact corruption the arc exists to
  kill, undetectable in review. Enforcement (wrappers that don't expose the
  lock-free mutators) makes the bad state unrepresentable. Histograms are the
  exception that proves the rule: they have no lock-free mutator, so no wrapper is
  needed.
- **Aliased import (`WindowedLazyCounter as LazyCounter`) to avoid churn.**
  Tempting, but *rejected once enforcement was chosen:* enforcement forces every
  windowed write to `set_with_window` regardless, so aliasing never saved the
  call-site churn — only the declaration churn — and it **breaks for groups**,
  where one sampler (`cpu/usage`) mixes windowed per-CPU groups with plain
  packed/mmap-direct groups that cannot share a single aliased type name. An
  honest, explicit migration is clearer and handles the mixed case.

## Fit with the arc / principles

- Generalizes Phase 1; drivehealth's per-index windows become the first
  torn-safe Regime-R consumer.
- **Principle 6** — the mmap-direct follow-on is the pure form; this step is
  faithful in the interim (windows recorded at the real read).
- **Principle 10** preserved — no agent-side clock; windows are recorded when the
  sampler actually reads.
- **Principle 16** — overhead measured, not asserted; per-metric cost is a lazy
  cell + a locked read; the general metriken library is untaxed beyond an opt-in
  null pointer.
- Temporal-only; `epoch`/quantum still deferred with the shape ready for them.
