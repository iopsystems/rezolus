# Measurement uncertainty — acquisition windows, multi-timeline plotting, and rate error bars

- **Opened:** 2026-07-08
- **Status:** OPEN — vision/intent landed, pre-build. **Cross-cutting arc**; core
  lands in **metriken**, rezolus is the first consumer. **Temporal-first** (see
  Scope). Delivered in phases, each its own entry → spec → PR.
- **Arc:** "Measurement uncertainty" — treat every observation as a *measurement*
  with a temporal acquisition window (and, later, a value quantum), carried
  end-to-end so we can plot heterogeneous-cadence samplers together honestly and
  put **error bars on rates**.
- **Owner:** Brian Martin

This entry is the durable design record (it absorbs the brainstorm; no separate
spec doc). Per-phase specs come later.

## The pressing problem

Today a metric value is a *point*, stamped with one `SystemTime::now()` captured
per refresh and shared by every metric (`src/agent/exposition/http/snapshot.rs`
lines 39/60/82). That "unified timestamp" is a myth in two ways; the **pressing**
one is temporal:

- **Different samplers sample at different instants, and some have large spread
  within a single collection.** `drivehealth` reads 23 devices over
  **0.18–2.3 s** (measured across runs, #992); each device completes at a
  different instant. Those values are written at the snapshot timestamp with no
  marker that they were acquired earlier, over a window.
- **Consequences:** you cannot honestly plot heterogeneous-cadence series on one
  axis, and rates carry no error bars even when they are wildly uncertain — a
  jiffy/tick-quantized CPU counter rated over a 1 ms window is honestly **±~100 %**
  (0 or 1 tick), yet today it reports a crisp number.

**Headline goals:** (1) drop the unified-timestamp myth and plot
different-instant samplers together honestly; (2) **error bars on rates.**

The value-uncertainty half (histogram bucket error, sensor precision) is real but
**secondary** — modeled so we don't overhaul twice, but deferred. The one piece
of value-uncertainty that rides along now is the **counter increment quantum**,
because a rate's error bar needs it (below).

## The model

```
observation = (value, acquisition_window[t_begin, t_end], kind,
               start_epoch?,        # cumulative kinds only
               value_quantum?)      # counter quantum now; histogram/gauge later
```

- **acquisition_window `[t_begin, t_end]`** — the interval over which the value
  was captured, as tight as the *physical* read (per device, per file). It is an
  **uncertainty interval, not a selectable instant** ("send vs receive" is not a
  choice; the value is uncertain *across* the window). Cheap co-sampled reads
  (mmap/BPF) share **one** near-zero window — the snapshot collection duration
  made honest — so they are not taxed per-metric.
- **kind** — *point* (gauge/sensor: window = uncertainty-of-instant) vs
  *cumulative* (counter/histogram: window = when the running total was sampled).
- **start_epoch** (cumulative only) — the accumulation/reset epoch, **distinct
  from the read window**. Required so `rate()`/`increase()` behave correctly
  across agent restart, counter wrap, or BPF map re-init. (This is what
  OpenTelemetry's start-time is; the read window is a *different*, latency
  interval — the two must not be conflated.)
- **value_quantum** — the measurement granularity:
  - *Counter* — the increment quantum, **only where the source is coarse**:
    tick-time (`cpuacct_account_field` is tick-driven; quantum = `TICK_NSEC`,
    i.e. 1/4/10 ms at HZ 1000/250/100 — the agent must *discover* HZ), sectors→
    bytes (`blockio` `nr_bytes`, 512 B), pages (4 KB). **Many BPF counters are
    exact** (syscall/context-switch/packet counts, quantum = 1 event) — for those
    the quantum is 1, not a lie. Needed now because it feeds rate error bars.
  - *Histogram* — structural (per-bucket relative width from the H2
    grouping-power, principle 8), surfacing only when a percentile is drawn
    downstream (principle 9). **Deferred**; do not force histograms into the
    scalar `value_quantum` field — they get their own treatment at query time.
  - *Gauge* — sensor/ADC resolution; **unknown ≠ zero** (treating unknown as
    negligible reintroduces exactly the false precision we set out to kill).
    Deferred.

**Rate error bars need both axes.** `rate = Δv/Δt`: the error bar comes from the
two read-window widths (Δt, temporal) **and** the counter quantum (Δv, value).
That is why the counter quantum rides in the temporal-first cut.

## Clocks (the honesty depends on this)

A window is only honest relative to a clock. We handle both host scopes:

- **Intra-host — monotonic.** All same-host samplers share the monotonic clock;
  windows are exact and same-host correlation (e.g. drive-temp vs. IO-latency on
  one box — the motivating case) is precise. Store one `(wall, monotonic)` anchor
  per recording; express windows in monotonic, place absolutely via the anchor
  (monotonic isn't serializable and resets across process restart, so it needs the
  anchor).
- **Cross-host — measured via NTP.** chrony/ntpd expose **offset, frequency
  drift, and root dispersion** (a real bound on absolute error). We carry that as
  a first-class **clock-uncertainty** term rather than pretending clocks agree.
  This is Cristian's algorithm / NTP round-trip bounding applied to telemetry.

## Correlation ceiling (precise definition)

The finest lag/alignment resolvable between two series is
`max(window_a, window_b)` intra-host, `+ clock_uncertainty(a, b)` cross-host. The
viewer/MCP **refuse or grey** correlations finer than that rather than fabricate
sub-window alignment. It is a resolution floor, not a slogan.

## Storage / format

- Drop "single dense row is the only shape." The **co-sampled fleet stays a dense
  table with one shared near-zero window** (the existing `duration`, made honest)
  — cheap counters are not pushed into long tables. Only genuinely
  heterogeneous/expensive-read cohorts (drivehealth today) get their **own
  timeline** — a long table, one row per (device, read) with its own window.
- **Common metriken archive** (working name `.mtk`/`.met`) =
  `tar(per-cohort parquet tables + manifest.json)`; projects brand the extension
  (rezolus → `.rez`) but it is the same format. Manifest carries the clock anchor,
  per-cohort `kind`, quanta, and global metadata (systeminfo/descriptions/source).
- **Store the honest inputs** — window endpoints, `start_epoch`, `value_quantum`,
  clock-uncertainty — **not a pre-combined σ**. You can derive either interval or
  statistical uncertainty downstream from the inputs; you cannot recover inputs
  from a collapsed σ. This also keeps the propagation-math choice at the query
  layer (below).
- **v2→v3 converter**: old file → single cohort, `window = [ts, ts+duration]`,
  kind=point, epoch = process start if known, quanta absent.

## Consumers

- **TSDB / query (`metriken-query`)**: store per-sample windows + epoch + quantum;
  `rate()`/`increase()` return a value **with an error bar** (from Δt windows,
  Δv quantum, and epoch handling); as-of join across cohorts on the anchored clock.
  **Back-compat is a design constraint:** the error-bearing return type must not
  break existing PromQL queries / dashboards / recording rules — plain queries
  keep working; the uncertainty is opt-in/side-channel. (Open item.)
- **Viewer**: plot heterogeneous-instant series together honestly; error bars /
  confidence bands on rates; enforce the correlation ceiling.
- **MCP**: confidence-aware anomaly/correlation.
- **Exporter (Prometheus)**: heterogeneous freshness breaks the scrape's
  single-instant contract regardless of projection; down-project to points and use
  OpenMetrics `_created` (the epoch) + staleness markers where possible.
  Documented lossy boundary; OTel export can preserve `[start,end]` for
  cumulatives.

## Propagation semantics (the fork, named — pin before the query phase)

Store bounds/inputs; choose the math at **query time**:
- **Interval (worst-case) bounds** — simple, but `sum()` over N series grows
  `O(N)`, degenerating at fleet cardinality.
- **Statistical (variance) propagation** — `O(√N)`, but needs a stated
  independence + distribution model (quantization is uniform-bounded, a window is
  a bound — not Gaussian; mixing them under one "uncertainty" is undefined until we
  choose).
Default worst-case for small N / correctness; statistical for aggregates under a
declared independence assumption. This dictates the query engine and must be
pinned before the query phase.

## Decision log

- **Temporal-first.** Drop the unified-timestamp myth; plot different-instant
  samplers together; **error bars on rates** = headline. Value-uncertainty modeled
  but deferred — **except** the counter increment quantum, which rides now (rate
  error bars need it).
- **Cross-host is in scope, measured** via NTP offset/frequency/root-dispersion —
  not deferred.
- Co-sampled fleet keeps **one shared near-zero window**; only heterogeneous
  cohorts split to their own timelines. Cheap counters untaxed.
- Record the **window** (both ends `[t_begin,t_end]`), never a chosen instant.
- Cumulative kinds carry `start_epoch` distinct from the window.
- **Store honest inputs (bounds), not σ**; defer interval-vs-statistical
  propagation to the query layer.
- Histograms get query-time percentile-uncertainty treatment, not a scalar
  `value_quantum`.
- Home = metriken; common archive format + branded extensions. metriken `next`
  branch or hard-fork; **no crates.io publish until design + migration are solid.**

## Phasing (temporal-first)

1. **Primitive + capture + wire.** `(window, kind, start_epoch, counter quantum)`
   in metriken; `drivehealth` captures tight per-device windows; extend exposition
   (`SnapshotV3`, minimal) so the windows are actually **visible on the snapshot**
   (the wire slice is part of Phase 1 — per-device windows are not observable
   without it). Includes reading HZ for the tick quantum.
2. **Archive + plot-together.** Common `.mtk`/`.rez` archive + recorder + v2
   converter; viewer plots heterogeneous cohorts on one axis (myth dropped).
3. **Rate error bars end-to-end** (headline). TSDB carries windows+quantum+epoch;
   `rate()`/`increase()` return error bars; correlation ceiling in the viewer.
   (May land on the live path before the archive, since it needs only the
   wire+TSDB, not the file format.)
4. **Cross-host clock uncertainty** from NTP (offset/frequency/root-dispersion) →
   cross-host correlation with honest resolution.
5. **Fuller value uncertainty** (histogram percentile bounds, gauge precision) +
   statistical propagation + MCP confidence — the "fancy stuff."

Order can flex; Phase 1 is next. Each phase gets its own journal entry → spec → PR.

## Fit with principles

- **Principle 10** holds for mmap cohorts (consumers drive read cadence; cohorts
  self-stamp their window); the Phase-1 pilot (`drivehealth`) is the sanctioned
  **principle-17** expensive-read exception, not a violation.
- **Principles 13 / 16** (overhead is budgeted and **measured**): by design the
  fleet is *not* taxed per-metric — co-sampled counters share one cohort window;
  per-row windows exist only for heterogeneous cohorts (drivehealth: ~23 rows per
  interval, trivial). **Target:** per-observation temporal overhead adds only the
  cohort window + epoch (a few u64 per cohort per snapshot) for the dense fleet;
  measure and report the number per principle 16 before shipping each phase.
- **Principle 8**: the H2 bounded relative error *is* the histogram value
  uncertainty (deferred, query-time).
- Subsumes the `drivehealth` staleness thread (#992) **only because windows are
  absolute-clock**: `age = now − t_end` (staleness, up to `interval`) is derived,
  and is a *different* axis from acquisition width (`t_end − t_begin`, the read
  latency). Both are carried.

## Non-goals / what this arc does not address

This arc makes telemetry honest about *when* and *how precisely* it was measured.
That is **one class** of problem. It is not a total telemetry-soundness effort,
and the next reader should not mistake "we designed the uncertainty model" for
"rezolus telemetry is now sound."

**Out of scope entirely:**
- **Sampling adequacy / aliasing.** Honest windows *quantify* uncertainty; they do
  not add data you did not sample. A phenomenon sampled slower than it changes is
  still aliased — the model surfaces that instead of hiding it, but the fix
  (higher rate, or in-kernel aggregation) lives elsewhere.
- **Data gaps / agent downtime.** Representing dropped snapshots, scrape failures,
  and restarts as *gaps* (vs. silent interpolation) is related but separate.
  `start_epoch` lets `rate()` survive restarts; gap semantics are not designed here.
- **Metric semantic correctness.** Whether a sampler measures the right thing
  correctly (accounting nuances, attribution) is orthogonal — this arc times and
  bounds whatever the sampler already produces.
- **Agent overhead reduction.** This arc can *add* cost (windows, epochs, quanta);
  it does not shrink the existing footprint. It commits to *measuring* the
  addition (principle 16), not to reducing the base.

**New problems this arc creates and must eventually answer (not solved here):**
- **Downsampling / retention of an uncertain series** — rolling up a value that
  carries an error bar has to *combine* the uncertainty, not drop it.
- **Alerting / thresholds on uncertain values** — does an alert fire on the mean,
  the optimistic bound, or the pessimistic bound? Uncertainty makes a threshold a
  policy decision.

**Already handled elsewhere in rezolus (not by this arc):**
- High-frequency phenomena → in-kernel BPF aggregation / H2 histograms
  (principles 3, 6, 8).
- Label cardinality / cost → sparse groups + cardinality discipline (principle 13).

## Prior art (this is a synthesis, not sui generis)

The ingredients are well-trodden; the novelty is the *synthesis* for an always-on
systems agent:
- **NTP / Cristian's algorithm** — `[t_begin,t_end]` instant-bounding *and* the
  residual clock-uncertainty term (root dispersion).
- **OpenTelemetry** start-time (counter epoch) + **Exemplars**; **OpenMetrics
  `_created`** and staleness.
- **RRDtool** heartbeat / `UNKNOWN` for stale; **Prometheus staleness markers**.
- **Interval / affine arithmetic** (Moore; Stolfi–de Figueiredo) — the
  interval-propagation branch.
- **Kalman filtering / sensor fusion** — carrying a measurement covariance and
  propagating it through derived quantities is the standard practice; this is that,
  for telemetry.
- **Uncertain / probabilistic databases** (MayBMS, MCDB); **TimescaleDB
  time-weighted aggregates**; scientific TS formats (FITS/HDF5) with per-sample
  uncertainty columns; **distributed-tracing spans** as `[start,end]` windows.

## Open questions / logistics

- **Propagation math** — interval vs statistical (pin before Phase 3/4); it
  dictates query semantics and what error-bar type `rate()` returns.
- **Query back-compat** — how the error-bearing return type coexists with plain
  PromQL so nothing breaks.
- **Clock plumbing** — reading NTP root dispersion (chrony `tracking` /
  `ntp_adjtime`) and the `(wall, monotonic)` anchor mechanism.
- **Archive** name/extension (`.mtk` vs `.met`) and manifest schema.
- **metriken governance** — `next` branch vs hard-fork; crates.io coordination
  with other metriken users is a real cross-team gate, not just a note.
- **Security/PII** — the archive manifest carries `systeminfo` and drivehealth
  serials (flagged sensitive in #992); confirm the new artifact adds no exposure
  beyond today's parquet and document the posture.
- **`kind` taxonomy** sufficiency (point vs cumulative — enough? rate-of? event?).
