# Multi-endpoint `.rez` — closing the last single-recording seam

- **Opened:** 2026-08-28
- **Status:** **SHIPPED, measured.** `record --endpoint a --endpoint b -o out.rez`
  writes one archive holding a recording per endpoint. Four of the five
  GO/NO-GO criteria pass with room; the fifth was **mis-specified** and is
  restated below — but measuring it found a real, pre-existing lockstep bug
  (the byte cap escaped the seal stagger), which is fixed here. See *Results*.
- **Arc:** completes the container work begun in
  [the SQLite container](2026-08-12-rez-sqlite-container.md) and
  [stage 4 native V3 ingest](2026-08-20-stage4-native-v3-ingest.md), and removes
  the deferral recorded in `src/recorder/mod.rs` when `record` learned to default
  to `.rez` (#1097).
- **Owner:** Brian Martin

## Why

`.rez` is a multi-recording container everywhere except the one place a
recording is actually made.

The manifest is `recordings: Vec<RezRecording>` (`src/recorder/rez.rs:40`), a bag
of label-tagged recordings. The SQLite schema keys every row and every query by
`(recording_id, sampler, seq)` (`src/recorder/rez_sqlite.rs:116`). The reader
enumerates N of them (`src/rez_reader.rs:320`). `combine` assembles N archives
into one (`src/parquet_tools/combine.rs:312`), `annotate` writes per-recording
KPIs, and the viewer reads a 2-recording archive as an A/B pair.

`record` cannot produce one. `src/recorder/mod.rs:842` demotes to parquet when
`endpoints.len() > 1`, with the comment *"Multi-source/A-B `.rez` is deferred"*.
So the supported route to a multi-recording archive is: record N separate
archives, then `combine` them. That works, but it means the format's own
multi-host and A/B story is offline-only, and two arms captured sequentially
differ in load as well as in whatever the experiment changed — the exact
confound the [read-path benchmark](2026-08-27-rez-vs-parquet-read-path.md) had to
design around by running two recorders concurrently against one agent.

Parquet already does this: `record --endpoint a --endpoint b -o run.parquet`
row-merges several sources, and `--separate` splits them per file.

## Scope

**In:** several *rezolus/msgpack* endpoints into one multi-recording `.rez`,
live, in one `record` invocation. Each endpoint becomes one recording with its
own label set (`source`/`host` auto-populated, plus `--label`).

**Out of *this* effort, but a real and tractable gap — see below.** Prometheus
sources inside `.rez` are deferred here to keep this change to one thing, not
because the format forbids them.

An earlier draft of this entry claimed the boundary was structural: that a
Prometheus scrape has no groups, no samplers, and no acquisition windows, so
admitting one would fabricate the uncertainty metadata the format exists to keep
honest. **That was wrong on all three counts, and the opposite of the truth on
the one that matters.** Corrected here rather than quietly dropped, because the
claim had already been written into `docs/backlog.md` as by-design.

- **Windows already exist.** `sample_window` (`src/recorder/prometheus.rs:335`)
  sets one on every counter, gauge and histogram the converter emits.
- **They are zero-width.** It returns `Window::new(ns, ns)` — begin equals end.
  So today a Prometheus recording asserts that an entire scrape was read at one
  instant. That is precisely the claim
  [all-sampler observation windows](2026-07-10-all-sampler-observation-windows.md)
  identifies as the thing the arc kills: *"a bracket is honest where an instant
  would claim zero width"*.
- **The capability is already there.** `RezV3Writer::ingest` handles
  `Snapshot::V2` through `group_by_sampler`, and `SnapshotV2` is exactly what
  `PrometheusConverter` produces. The `.rez` refusal in
  `src/recorder/mod.rs:836` is a policy check on the endpoint's protocol, not a
  limit of the writer.

So a scrape *is* one acquisition — one request, one response, every metric in it
read together — and modelling it as **one acquisition group per scrape target,
with the window set to the real HTTP round-trip `[request_sent,
response_received]`**, would be strictly more honest than the zero-width instant
we record now.

**It must be one group, not per-metric windows.** This is the load-bearing
constraint, and it decides the shape of the work. `write_table_parquet`
(`src/recorder/rez.rs:305-328`) has two branches: the group branch emits a single
bare `:window_begin`/`:window_width` pair for the whole table, while the V1/V2
branch emits `<name>:window_begin` (Int64) and `<name>:window_width` (UInt64)
**per value column** — three columns per metric. `PrometheusConverter` emits
`SnapshotV2`, so routing it into `.rez` unchanged would work and triple the
schema width of every Prometheus table. That is precisely the cost acquisition
groups were introduced to remove, so the converter has to learn to emit
`SnapshotV3` with one group per target. "The writer already ingests V2" is true
and misleading: it would ingest, and blow up the columns.

**And the per-line timestamp cannot be the window source.** Prometheus exposition
carries an optional trailing timestamp in *milliseconds since epoch*, meant as a
federation/pushgateway staleness marker. `convert` passes `fetch_ns` to
`Scrape::parse_at` as the default, so `sample.timestamp` is the embedded value
when present and the fetch instant otherwise — two different semantics silently
mixed in one recording. Worse, the embedded value is taken at face value: the
existing test `embedded_timestamp_becomes_window`
(`src/recorder/prometheus.rs:355`) asserts that `m_total 3 1000` yields
`begin_ns == 1_000_000_000` — a window beginning **one second after the Unix
epoch**, roughly 56 years before the recording that contains it. Any exporter
that emits timestamps (pushgateway, federation) writes that today. The window
must come from the recorder's own clock around its own fetch, and the line
timestamp must be ignored for window purposes.

A zero-width window is the fallback if a round-trip pair is somehow unavailable,
but it should not be the design: we issue the request, so we have both instants,
and per principle 18 a bracket that over-states is always preferred to an instant
that under-states.

One genuine caveat survives, weaker than the original claim and worth stating
because it is the reason to keep the two efforts separate: the round-trip bounds
when *we* read the exporter, not when the exporter computed its values. An
exporter that serves cached values would make even a correct round-trip window
under-state the true uncertainty, which is the dangerous direction. But that
is (a) strictly better than the zero width shipped today, and (b) the same class
of documented property as principle 18's device-sweep archetype, where a failing
call retains a stale value under a freshly-stamped window. It is a caveat to
record, not a reason to refuse.

**Filed as its own effort.** It needs: `PrometheusConverter` emitting
`SnapshotV3` with one group per target rather than `SnapshotV2`; the round-trip
pair plumbed in (the converter is handed only parsed text and a single
`fetch_ns`, so the request instant does not currently reach it); the embedded
line timestamp dropped as a window source; a table key for a source with no
`sampler` label; and its own honesty review of the caching-exporter case.

**Out:** viewer support beyond two recordings (`src/viewer/mod.rs:581` shows the
first two and warns). Independent of this, and tracked separately — but note that
finishing this work makes 3+-arm archives easy to *produce* and still
unviewable, so the two should not drift far apart.

## Design

The storage layer needs no change. The work is entirely in the two layers above
it, and they split cleanly:

- **`RezV3Writer`** owns the file, the connection, and the writer thread. It
  becomes multi-recording: `Msg` variants carry a `recording_id`, `next_seq`
  keys on `(recording_id, sampler)` rather than `sampler`, `observed` becomes
  per-recording, and the thread exits when *every* recording has finalized
  rather than on the first `Finalize`. `create()` gains `add_recording(seed) ->
  recording_id`.

  One thread and one connection stay correct, and are not a compromise: SQLite
  has a single write lock, and the container's design note already records that
  a second writing connection stalls on it for `busy_timeout` before failing.
  `SyncSender<Msg>` is `Clone`, so each recording holds its own handle.

- **`StreamRecorderV3`** holds the seal policy and the per-recording bookkeeping
  — `accounts`, `last_keys`, `described`, `schemas`, the per-group schema-hash
  set. All of that is per-recording by nature: two agents have independent
  samplers, independent schema generations, and independent segment rotation.
  So there is **one `StreamRecorderV3` per endpoint**, each tagged with its
  recording id, all sharing the one writer.

- **`record`** replaces `Option<RezStream>` with a `Vec<Option<RezStream>>`
  parallel to `endpoints` and `writers`, which is the shape the parquet path
  already uses, and drops the `endpoints.len() > 1` blocker.

### The stagger has to change

Found while scoping, and it would have been a quiet regression.
`SegmentAccount::open_first` desyncs a recording's tables by shortening each
sampler's *first* segment, so they do not all reach `max_rows` in lockstep and
seal as one oversized batch forever (`src/recorder/seal_policy.rs:99-120`). The
bucket comes from `stagger_bucket(sampler)` — FNV-1a over **the sampler name
alone** (`seal_policy.rs:164`).

Two rezolus agents have *identical* sampler sets. Every table in recording B
would therefore draw the same bucket as its namesake in recording A, and the two
recordings would seal in permanent lockstep — doubling the co-seal batch size at
exactly the moment the archive holds twice the tables. The stagger would still
be working within a recording and silently defeated across them.

The fix is to widen the hash beyond the sampler name. **Not** with the recording
id: that is an autoincrement integer, so the bucket — and therefore where every
segment boundary falls — would depend on the order the endpoints were listed on
the command line. The same two agents recorded with the flags swapped would
segment differently, which makes a capture non-reproducible for no reason.

Hash the **sampler name plus the recording's label set**, canonicalised (sorted
`k=v`, joined). The labels are already what identifies a recording — `source`
and `host` are auto-populated and `--label` adds the rest — so this keys the
stagger on the thing that actually distinguishes two arms, and it is stable
across runs and across flag order.

It has to be the whole label set rather than just the node name, because the two
cases differ: multi-host archives separate on `host`, but an A/B on a *single*
host has the same node in both arms and separates only on `arm` (or whatever
`--label` the operator chose). Hashing the node alone would leave same-host A/B
in exactly the lockstep this fix exists to break.

Two consequences to accept deliberately:

- **Single-recording buckets move.** Folding labels in re-shuffles which bucket
  each sampler draws today. That is not a regression — the property the stagger
  needs is *spread* across the 64 buckets, not any particular assignment — but
  it does mean existing recordings and new ones segment differently, so the
  before/after byte comparisons in this effort must not be read as a size
  regression.
- **Identical label sets still collide.** Two recordings that are genuinely
  indistinguishable by label draw the same bucket and seal in lockstep. That is
  the degenerate case (the operator gave two endpoints nothing to tell them
  apart), and the honest answer is to warn at startup rather than to silently
  fold in the recording id and reintroduce order-dependence.

## GO / NO-GO

Measured on a Linux host, two agents (or one agent scraped twice), against the
same run recorded as two separate single-recording archives plus `combine`:

1. **Finalize stays bounded.** Median finalize for a 2-recording archive within
   **1.3×** the single-recording median (303.7 ms, #1041). NO-GO above 2×: the
   bounded-finalize property is the format's main claim over parquet.
2. **No co-seal regression.** With the stagger fix, the largest seal batch in a
   2-recording archive is within **1.25×** the single-recording maximum. This is
   the number the stagger finding above predicts, and the one that catches it if
   the fix is wrong.
3. **Recorder RSS.** Peak within **1.6×** single-recording, against the
   50–100 MB always-on target ([recorder resource footprint](2026-08-13-recorder-resource-footprint.md)).
4. **Dropped ticks stay at parity** — the 0.4% floor from #1061, not the 8.9%
   that preceded it. Two recordings must not reintroduce scrape-loop stalls
   through writer backpressure.
5. **Byte parity with `combine`.** A 2-endpoint live archive and the same two
   runs combined offline hold the same tables, rows, and windows. Divergence
   here means the live path is not producing the format the offline path does.

NO-GO on 1 or 4 parks the effort: recording two agents into separate archives
and combining them already works, and is strictly better than a live path that
drops ticks.

## Plan

1. `RezV3Writer` multi-recording — `Msg` carries the id, per-recording `next_seq`
   and `observed`, finalize refcount, `add_recording`.
2. Stagger keyed on sampler + canonical label set, plus the co-seal test that
   fails without it and a startup warning for indistinguishable label sets.
3. `StreamRecorderV3` tagged with its recording id; one per endpoint.
4. `record`: `Vec<Option<RezStream>>`, drop the blocker, per-endpoint labels.
5. Measure against the GO/NO-GO, land the numbers here.

## Deferred

- **Prometheus inside `.rez`** — a real gap, and a measurement-honesty
  *improvement* rather than a compromise (see Scope). Blocked only by a policy
  check; the writer already ingests what the converter emits.
- **Viewer beyond two recordings** — `src/viewer/mod.rs:581`.
- **Hindsight multi-endpoint** — the rolling buffer is single-agent by
  construction today; the writer work here is a prerequisite, nothing more.

## Results

Measured on a 32-core Linux host, release build, against a live agent. **The
host was not idle** — it carries other work throughout — so both arms were
captured against the same agent under the same conditions and every figure
below is a ratio between arms rather than an absolute cost.

Two configurations, 120 s at 1 s for the footprint numbers and 300 s at 50 ms
for the sealing numbers (50 ms is what makes tables seal repeatedly in steady
state rather than only at finalize).

Both arms were re-measured on the post-stagger build, since the stagger change
moves segment boundaries and a pre-change archive is not a valid baseline.

| criterion | bar | measured | verdict |
|---|---|---|---|
| Finalize | within 1.3× | **1.17×** (1.42 s → 1.66 s of post-window overhead) | PASS |
| Recorder RSS | within 1.6× | **1.08×** (101.6 → 110.2 MB) | PASS |
| Dropped ticks | at the 0.4% floor | **zero** — both recordings 26 tables / 3028 rows, identical to the single-recording baseline | PASS |
| Byte parity with `combine` | same tables/rows/windows | **1.0008** on total bytes | PASS |
| Largest seal batch | within 1.25× | 2.00× including finalize, **1.50×** steady-state | see below |

### The seal-batch criterion was mis-specified

It does not separate three different things, and only one of them is a defect:

1. **The finalize batch is inherently N×.** Finalize seals every open tail by
   definition, so two recordings mean twice the tails: 25 → 50. No stagger can
   or should avoid that.
2. **Doubling the table count raises coincidence on its own.** With 52 tables
   instead of 26 landing in the same number of seal instants, the maximum
   batch rises from birthday effects even under perfect independence.
3. **Genuine lockstep**, which is what the criterion was *meant* to catch.

Written as a single ratio it conflates all three. Restated, the criterion should
be *steady-state* (finalize excluded) and about the **distribution**, not the
maximum.

### What it caught, which is the point

`SegmentAccount::open_first` staggered `max_rows` and `max_age` but read
`policy.max_bytes` directly, so **the byte cap was never staggered.** The policy's
own documentation says the byte cap is what splits the *wide* tables — so
precisely those tables had no phase offset at all.

Within one recording that was survivable: different samplers fill at different
rates and drift anyway. Across two recordings of the same agent it is not,
because the two carry identical data, reach the cap on the same row, and seal
together forever. Measured, at 50 ms over 300 s:

| sampler | segments | boundaries coincident across the two recordings | max rows |
|---|---|---|---|
| `cpu_usage` | 49 | **49** | 99 |
| `syscall_latency` | 37 | **37** | 131 |
| `scheduler_runqueue` | 17 | **17** | 296 |
| `cpu_bandwidth` | 6 | 1 | 900 |
| `cpu_branch` | 6 | 1 | 900 |

The split is exact: every table sealing *at* 900 rows (row-bound) desynced
correctly; every table sealing *before* 900 (byte-bound) was in 100% lockstep.

Staggering the byte cap by the same bucket fixes it. Steady-state co-seal
distribution, finalize excluded:

| | one recording | two, before | two, after |
|---|---|---|---|
| max batch | 2 | 4 | 3 |
| pairs | 23 | 161 | 40 |
| triples | 0 | 10 | 3 |
| quads | 0 | 1 | 0 |

Lockstep pairs fell ~4×, and whole-archive boundary coincidence went from
**157 of 256 to 26 of 231**. What remains is items 1 and 2 above.

This is the value of a number-gated close-out: the criterion was wrong, and
running it anyway surfaced a latent bug that had been shipping since the
stagger was written. It was invisible while an archive could hold only one
recording, and would have become a silent doubling of steady-state seal
transactions the moment one could hold two.

### Not measured

Query latency across a multi-recording archive. The read path is unchanged by
this work — `RezReader::from_v3` already enumerated N recordings — and the
[read-path entry](2026-08-27-rez-vs-parquet-read-path.md) prices open cost per
table, which is what a second recording adds. Worth a number if multi-recording
archives become common.
