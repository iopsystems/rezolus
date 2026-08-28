# Multi-endpoint `.rez` — closing the last single-recording seam

- **Opened:** 2026-08-28
- **Status:** **OPEN — intent landed, not yet built.**
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

One genuine caveat survives, weaker than the original claim and worth stating
because it is the reason to keep the two efforts separate: the round-trip bounds
when *we* read the exporter, not when the exporter computed its values. An
exporter that serves cached values would make even a correct round-trip window
under-state the true uncertainty, which is the dangerous direction. But that
is (a) strictly better than the zero width shipped today, and (b) the same class
of documented property as principle 18's device-sweep archetype, where a failing
call retains a stale value under a freshly-stamped window. It is a caveat to
record, not a reason to refuse.

**Filed as its own effort.** It needs the window plumbed from the fetch (the
converter currently sees only the parsed text and a `fetch_ns`), a decision on
the table key for a source with no `sampler` label, and its own honesty review of
the caching case.

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
