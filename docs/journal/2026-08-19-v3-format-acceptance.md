# V3 format acceptance — wave-1 measurement

- **Opened:** 2026-08-19
- **Status:** **MEASURED**. Verdict: format holds; one wire-protocol gap
  found and quantified (see Verdict below).
- **Arc:** the acceptance gate for the acquisition-groups redesign's wire
  format, following [acquisition-window sidecars](2026-08-17-window-sidecar-cost.md)
  (identified the sidecar/column-count problem SnapshotV3 exists to solve)
  and the [split-table read-cost gate](2026-08-18-split-table-read-cost-gate.md)
  (passed the `.rez` layout half of the redesign). This entry measures the
  other half — the transport format wave-1 sampler migration actually
  produces — before more stages (histogram wave, allocation-parity work,
  recorder-side V3 ingest) build on top of it.
- **Owner:** Brian Martin
- **Repos:** rezolus, branch `feat/agent-acquisition-groups`
  (commit `1110a25`); metriken fork `brayniac/metriken`, branch `acq-groups`
  (rev `f601f48`, git-pinned in `Cargo.toml` while the format iterates).

## Why

Stage 3c wave 1 group-stamped ten samplers' BPF counter machinery (14
acquisition groups, 104 tagged metrics) behind `[general] snapshot_format =
"v3"` (default stays `"v2"`). Before wave 2 (more samplers) or Stage 4
(recorder-native V3 ingest) commit further to this format, its three central
bets need evidence, not code-shape reasoning: that acquisition windows on
migrated groups are real and µs-scale, that the C2 possible-CPU-sweep fix
actually bounds group membership to real hardware instead of `MAX_CPUS =
1024`, and that content-hashed schema caching (the mechanism the whole
design leans on to avoid re-shipping metric descriptors every tick) is
paying off on the wire — not just in the agent's own cache.

## Method

All measurements ran the release binary (`cargo build --release`) from a
build clone on a 32-core x86_64 host, Linux 6.12, tmpfs working directory,
under a steady background workload, against a separate build/config/port
from the host's own production agent (untouched throughout).

**A — Principle-16 sampling latency.** Per docs/principles.md §16: for
`cpu_usage` and `blockio_requests` separately, an agent with only that
sampler enabled, `[log] level = "debug"`, run under `sudo`; `/metrics/binary`
scraped once/second for 60s; the `<sampler> sampling latency: N us` debug
lines parsed for min/median/max. Each sampler run twice — `snapshot_format =
"v3"` and default (`"v2"`) — to confirm empirically that the refresh path
(the expensive part: the BPF map read) is format-independent, since only the
exposition layer changes between v2/v3.

**B — V3 payload inspection.** One agent, all samplers enabled (default
sampler config, only `listen`/`snapshot_format` overridden), 120
one-second scrapes of `/metrics/binary`, each tick's raw bytes saved to
disk; repeated in `v2` mode for the same duration as the wire-size
baseline. Decoded with `python3-msgpack` (installed via `apt-get`, no
network pip available on the host) against the pinned metriken-exposition
source (`GroupSnapshot`/`SnapshotV3` in
`metriken-exposition/src/snapshot.rs`, confirmed by inspecting one decoded
tick's structure before writing the analysis): each v3 tick is
`[systemtime, duration, metadata, groups]`; each group is `[name,
(hash_hi, hash_lo), schema, window, counters, gauges, histograms]`.

**C — Overhead quick-compare.** During each 120-scrape run (B), `ps -o
pid,rss,%cpu,etimes` sampled ~11 times across the run for the agent
process. Not a full A/B — flagged only for anything alarming.

## Results

### A — Principle-16 sampling latency (µs, n=60 refreshes each)

| sampler | format | min | median | max | mean |
|---|---|---|---|---|---|
| `cpu_usage` | v2 | 44 | 74.5 | 576 | 90.2 |
| `cpu_usage` | v3 | 45 | 75 | 345 | 86.1 |
| `blockio_requests` | v2 | 30 | 39 | 92 | 41.5 |
| `blockio_requests` | v3 | 30 | 38 | 67 | 38.4 |

v2/v3 medians agree within 1 µs for both samplers; the v2 max outliers (576
µs, 92 µs) are single-tick scheduling noise, not a systematic v2 cost — the
v3 runs show the same shape at lower peaks. **Confirmed: the refresh path is
format-independent**, as expected — v2/v3 only change what the exposition
layer does with the same map read.

### B — Wire payload, v3 vs v2 (bytes/tick, n=120 scrapes each)

| | min | median | max | mean |
|---|---|---|---|---|
| v3 | 262,728 | 358,065.5 | 385,311 | 352,299.5 |
| v2 | 211,512 | 321,433.5 | 348,157 | 310,070.4 |
| ratio (v3/v2) | | **1.114×** | | 1.136× |

**Where the extra bytes go:** across five sampled ticks spanning the run,
**89.4%–91.6% of every v3 tick's bytes is the `schema` field**, not values —
measured by re-encoding each group's `schema` alone and summing against the
whole tick's repacked size (see Findings).

### B — Cardinality (entries/tick, n=120 each)

| | min | median | max | mean |
|---|---|---|---|---|
| v3 (schema-declared slots, all non-null this run) | 2,257 | 2,797.5 | 2,937 | 2,760.3 |
| v2 (counters+gauges+histograms) | 1,590 | 2,298.5 | 2,461 | 2,225.9 |
| ratio (v3/v2) | | **1.217×** | | 1.240× |

### B — Migrated groups (14 groups / 10 samplers, all 120 ticks)

Window present in every tick for every migrated group. Width distribution
(ns) and member count (constant per group across all 120 ticks):

| group | width min | width median | width max | members |
|---|---|---|---|---|
| `blockio_requests/blockio_requests_counters` | 580 | 945 | 3,150 | 8 |
| `blockio_requests/blockio_requests_errors` | 1,000 | 1,515 | 6,920 | 28 |
| `blockio_requests/blockio_requests_requeues` | 450 | 715 | 2,810 | 4 |
| `cpu_migrations/cpu_migrations_migrations` | 420 | 650 | 2,410 | 64 |
| `cpu_tlb_flush/cpu_tlb_flush_events` | 680 | 1,070 | 24,660 | 192 |
| `cpu_usage/cpu_usage_cpu` | 520 | 875 | 13,400 | 96 |
| `cpu_usage/cpu_usage_softirq` | 980 | 1,610 | 38,340 | 320 |
| `cpu_usage/cpu_usage_softirq_time` | 680 | 1,190 | 32,460 | 320 |
| `network_interfaces/network_interfaces_counters` | 450 | 870 | 2,030 | 4 |
| `network_traffic/network_traffic_counters` | 480 | 840 | 4,040 | 4 |
| `scheduler_runqueue/scheduler_runqueue_counters` | 470 | 815 | 7,620 | 128 |
| `syscall_counts/syscall_counts_counters` | 570 | 865 | 3,140 | 16 |
| `tcp_retransmit/tcp_retransmit_counters` | 310 | 530 | 4,600 | 1 |
| `tcp_traffic/tcp_traffic_counters` | 430 | 820 | 2,450 | 4 |

Every per-CPU group's member count is a small multiple of 32 (this host's
real core count: 64=32×2, 96=32×3, 128=32×4, 192=32×6, 320=32×10) — **never
1024**, confirming the C2 possible-CPU-sweep clamp (commit `1110a25`) is
doing what it claims on live hardware, not just in the unclamped-panics
regression test.

Schema-hash churn (fraction of tick-to-tick pairs where `(hash_hi,
hash_lo)` changed) is **0.0000 for all 14 migrated groups** across all 119
compared pairs each.

### B — Default (`snapshot_format = "v3"`, unmigrated `<sampler>/main`) groups

All 21 default groups: **window absent in every tick, every group**
(`window_present=False`) — confirmed, as designed for windowless/derived
groups pending migration.

Schema-hash churn is not uniformly zero, quantifying the known transitional
cost of dense-but-undeclared membership:

| group | churn |
|---|---|
| `cpu_usage/main` | **0.8487** |
| `syscall_counts/main` | 0.2437 |
| `scheduler_runqueue/main` | 0.2269 |
| `cpu_tlb_flush/main` | 0.1429 |
| `cpu_migrations/main` | 0.1008 |
| `cpu_perf/main` | 0.0756 |
| all other 15 default groups | 0.0000 |

`cpu_usage/main`'s member count ranged 235–667 across the 120-tick run
(per-task/per-cgroup membership drifting with the live workload); every
change in that set changes the schema hash, so nearly 85% of ticks resend
`cpu_usage/main`'s schema. This is exactly the population that migration
fixes: `cpu_usage`'s own migrated groups (`cpu_usage_cpu`,
`cpu_usage_softirq`, `cpu_usage_softirq_time`) sit at 0.0000 churn in the
same run.

### C — Overhead quick-compare (steady-state, last 5 of ~11 `ps` samples)

| | RSS | CPU% |
|---|---|---|
| v3 | ~89.3–91.7 MB | 2.1–2.4% |
| v2 | ~68.7–69.0 MB | 3.5–3.9% |

v3 steady-state RSS is **~1.33× v2** (91.6/69.0 MB) — consistent with, and
about at the low end of, the previously-flagged 1.2–3.4× allocation
overhead on cache hits (design notes, Stage 3c). CPU% reads lower for v3 at
steady state, but this is a single un-repeated `ps`-sampled run, not a
formal A/B — noted, not claimed.

## Findings

**Windows and member bounds hold.** Every migrated group emits a real,
present window on every tick, sub-2µs median width (300 ns–2.2 µs across
the 14 groups), with rare tail spikes to 24–38 µs under contention — the
same shape reported pre-migration for whole-sampler spans, just correctly
scoped per acquisition now. Member population tracks real hardware (32
cores × a small per-group multiplier), never `MAX_CPUS`. Schema-resend
churn for migrated groups is exactly the ≈0 the design predicted; the
unmigrated `/main` groups' churn (up to 85% for `cpu_usage`) is the
transitional cost migration is meant to remove, now with a number attached
instead of a description.

**The wire format is not yet winning on bytes, and the reason is
mechanical, not structural.** V3 ships **1.11–1.14× more bytes/tick** and
**1.22–1.24× more entries/tick** than v2 on the same recording. The entries
gap is the documented, intentional tradeoff: V3 sends every declared
member's real value (including zeros) where V2 sentinel-suppressed
zero/absent readings, so V3's cardinality is honestly higher for the same
underlying population — expected, not a bug. The *bytes* gap has a
different, fixable cause: **`src/agent/exposition/http/snapshot.rs:957`
sets `schema: Some(schema)` unconditionally on every group, every tick**,
and measured schema bytes are **89–92% of total v3 tick size** — despite
migrated-group schema hashes never changing across the entire 120-tick run.
The format supports exactly the fix (`schema: Option<GroupSchema>`,
content-addressed by `(name, hash)`, `GroupSnapshot`'s own doc comment
distinguishes "producers may always include it (stateless)" from a receiver
skipping *parsing* on a hash match) — but nothing today skips *sending* it.
`/metrics/binary` is stateless pull HTTP with no per-consumer cache-state
channel, so the schema-omission the design leans on for wire-size wins has
no protocol hook yet on the producer side.

## Verdict

**The pinned format holds.** Windows are real and correctly scoped,
member-population bounds are validated on live 32-core hardware (not a
synthetic test), schema-hash churn behaves exactly as designed for migrated
groups, and the refresh-path cost the format rides on top of is unchanged
(Measurement A). Nothing here blocks continuing wave 2 (more samplers) on
the current wire shape.

**What should change on the fork branch, before wave 2 finishes:** the
producer needs a way to *not* resend an unchanged schema, or V3's wire-size
story stays worse than V2's for as long as any consumer polls stateless
HTTP. Two directions, not mutually exclusive:

1. A conditional-fetch hook on `/metrics/binary` — the client sends the
   `(name, hash)` pairs it already has (a header or query param), the
   producer omits `schema` for those groups. Fixes the wire cost for any
   polling consumer (exporter, recorder) without changing the format.
2. Accept full-schema-every-tick as correct for genuinely stateless
   consumers (a one-shot scrape, an unknown client) but have the *known*
   long-lived consumers (recorder, exporter) open a connection mode that
   tracks server-side per-connection cache state instead of re-deriving it
   from a stateless request each time.

Until one of these lands, **V3's byte-for-byte win only becomes real for
consumers that read the `.rez` split-table layout** (the gate this arc
already passed, 0.55–0.68× query / 0.63× bytes) — the agent-to-exporter and
agent-to-recorder *wire* hop is currently a regression, quantified here at
~90% wasted bytes on schema resend. Recommend: file the conditional-fetch
mechanism as the next piece of Stage 3 infrastructure, run this same B/C
measurement again once it lands, and only then treat wire size as a closed
question for the flip-the-default stage.

Cardinality (v3 1.22–1.24× v2 entries) and RSS (v3 ~1.33× v2 steady-state)
are both already-known, already-explained costs (declared-vs-suppressed
membership; per-tick schema/value allocation) — this run adds live
20-plus-sampler numbers to them but changes neither the explanation nor the
backlog: the deferred rolling-hash-membership and schema-by-reference work
already named in the Stage 3c wave-1 record is what has to close them.

## Addendum (2026-08-19): schema-omission deferred — compression closes the wire gap

The wire question above got its answer the same day, in two parts.

**Measured: gzip collapses the schema overhead.** Same agent build, same
host, one scrape each way (`Accept-Encoding: gzip` against the endpoint's
existing `CompressionLayer`):

| | raw bytes | gzip bytes | ratio |
|---|---|---|---|
| v2 | 166,572 | 21,262 | 7.8× |
| v3 | 216,496 | 23,700 | 9.1× |

The compressed v3-vs-v2 delta is **+2.4 KB/tick (+11.5%)** — the
schema-resend regression effectively disappears for any consumer that
negotiates compression, because the schema section is exactly the kind of
repetitive text gzip eats.

**Decided: the conditional-fetch mechanism is deferred, with its design
banked.** Accounting for who actually pays the uncompressed bytes: loopback
consumers (the deployment model) pay ~nothing and their real cost —
re-parsing schemas — is already solved by receiver-side hash-skip; the
agent's serialize cost is a memcpy-shaped encode of a cached structure; raw
recordings pay ~11% but raw is a niche format that native `.rez` ingest
obsoletes; only remote-scrape fleets would genuinely pay, and that is not
the current topology. Revisit if it becomes one.

The banked design, should it be needed: the consumer announces, per group,
the ONE hash it currently holds (the schema it needs to decode anyway —
working state echoed, not new state); the agent omits schemas whose current
hash matches, includes the rest. No history is retained on either side —
PID wraparound means churned schemas essentially never recur, so a hash
*set* grows forever and never hits; only current-vs-current comparison is
worth anything. The agent stays stateless; a consumer that lost its cache
announces nothing and gets full payloads. **Hard caveat:** this is a
transport optimization only — any path that persists payload bytes verbatim
(the raw recording format) must not use it, or the stored payloads become
undecodable without stream context.

## Addendum (2026-08-19): histogram wave + V2 window restoration re-measured

Re-ran a lighter pass on the same host after five more commits landed
(histogram-group migration for 8 samplers, a V2 window-regression fix, and a
group-granularity collapse rule) — branch tip `78e69e38`. Same rules:
production agent/exporter untouched, isolated ports, 60-scrape captures.

**1. V2 windows restored on migrated groups.** Decoded 20 default-format
(v2) ticks, all samplers, and checked the trailing per-entry window on every
counter/gauge/histogram, grouped by its `sampler` metadata. Samplers with no
unmigrated remainder show a window on **100%** of entries (`blockio_latency`,
`blockio_requests`, `network_interfaces`, `network_traffic`,
`syscall_latency`, both `tcp_*_latency` samplers, `tcp_receive`,
`tcp_retransmit`, `tcp_traffic`) — matching drive-health/memory samplers that
were windowed independently of this migration and remain unaffected
(`drivehealth`, `memory_meminfo`, `memory_vmstat`, `cpu_cores`, all 100%).
Samplers still carrying an unmigrated remainder alongside migrated groups
show a fraction, not 100%, and it tracks the split exactly: for
`cpu_usage`, the migrated per-CPU `cpu_usage`/`softirq`/`softirq_time`
metrics carry a window on every entry while `cgroup_cpu_usage`,
`cgroup_cpu_usage_exited_tasks`, and `task_cpu_usage` (still unmigrated, per-
task/per-cgroup) carry none — confirmed by inspecting one tick's metadata
directly, not just the aggregate fraction. Genuinely unmigrated samplers
(`cpu_dtlb`, `cpu_frequency`, `cpu_l3`, `cpu_perf`, `rezolus_rusage`) are
0% windowed, unchanged from before wave 1. The regression fix restores
exactly the windows the migrated groups declare — no more, no less.

**2. V3 histogram groups present, windowed, stable.** A v3-flagged
all-samplers run decoded to **39 total groups: 25 migrated (non-`/main`) +
14 default**. All 11 target histogram-wave groups
(`blockio_latency_latencies`, `blockio_requests_sizes`,
`scheduler_runqueue_{runqlat,running,offcpu}`, `syscall_latency_latencies`,
`tcp_{connect,packet}_latency_latency`, `tcp_receive_{srtt,jitter}`,
`tcp_traffic_sizes`) carried a window on all 60 ticks, median widths
0.9–9.8 µs (tail up to 48.7 µs for `syscall_latency_latencies`, the widest
per-syscall-type sweep), and **schema-hash churn 0.0000** across every
group — same behavior as the wave-1 counter groups. All 25 migrated groups
(counter + histogram) were windowed on every tick with zero churn.

**3. No blow-up.** v3 bytes/tick: median 335,557 (was 358,065.5 in the
wave-1 120-scrape run); entries/tick: median 2,666.0 (was 2,797.5). Both
*lower* than the prior run despite 11 more groups — the fixed-size histogram
groups add only ~33 declared slots combined, and the dominant term is still
the dynamic `/main` groups' live task/cgroup population, which varies run to
run with the workload. Same ballpark, no regression.

## Addendum (2026-08-19): wave 2 — full migration measured

Re-ran after wave 2 (packed/sparse reader-stamped groups, memory, `cpu_cores`,
drivehealth's per-drive sweep, gpu) — branch tip `6f59031`, full rebuild
(BPF sources changed). Same host, same rules. Captures: 120-scrape v3, plus a
30-scrape v2 spot check.

**1. Default-group retirement — not complete.** Six `/main` groups remain
of 53 total:

| group | members | verdict |
|---|---|---|
| `cpu_bandwidth/main` | 2 | expected residue (the two ringbuf gauges) |
| `rezolus_rusage/main` | 9 | expected residue (ambient) |
| `cpu_dtlb/main` | 32 | **missed migration** |
| `cpu_frequency/main` | 96 | **missed migration** |
| `cpu_l3/main` | 64 | **missed migration** |
| `cpu_perf/main` | 64 | **missed migration** — only `cpu_perf`'s cgroup counters moved (`cpu_perf_cgroup_cycles`/`_instructions`); its base per-CPU cycles/instructions counters are still here |

Only two of the six are the accounted-for residue. `cpu_dtlb`, `cpu_frequency`,
`cpu_l3`, and `cpu_perf`'s base counters are genuinely unmigrated — the
"everything" framing for wave 2 does not hold for these four.

**2. Reader-stamped groups — present, windowed, and split into two width
regimes.** All cgroup-scoped and the one task-scoped group carried a window
on every one of 120 ticks. Widths separate cleanly from the sub-2 µs
sampler-stamped groups measured in earlier rounds:

| group | width median | width max | members | schema churn |
|---|---|---|---|---|
| `cpu_bandwidth_cgroup_{bandwidth_periods,throttled_periods,throttled_time,throttled_count,cgroup_throttled_time}` (5 groups) | 940–1,320 ns | 2,280–4,450 ns | 1 each | 0.0 |
| `cpu_migrations_cgroup` | 34.6 µs | 479.0 µs | 21–36 | 0.084 |
| `cpu_perf_cgroup_cycles` / `_instructions` | 41.1–48.5 µs | 180.2–610.6 µs | 48–65 | 0.101 |
| `cpu_tlb_flush_cgroup` | 128.0 µs | 480.5 µs | 150–255 | 0.118 |
| `cpu_usage_cgroup_usage` / `_cgroup_exited` | 24.3–87.1 µs | 120.6–309.2 µs | 26–104 | 0.193 |
| `scheduler_runqueue_cgroup_{context_switch,offcpu,wait}` | 25.8–55.4 µs | 120.4–213.2 µs | 47–130 | 0.109 |
| `syscall_counts_cgroup` | 410.0 µs | 1,769.4 µs | 544–960 | 0.168 |
| `cpu_usage_task` | 718.3 µs | 3,240.7 µs | 398–773 | **1.0** |
| `cpu_cores_read` | 147.0 µs | 572.0 µs | 1 | 0.0 |
| `memory_meminfo_read` / `memory_vmstat_read` | 134.5 / 171.0 µs | 543.9 / 650.1 µs | 5 / 6 | 0.0 |

`cpu_bandwidth`'s cgroup groups read fast (sub-2 µs, one-shot ringbuf-style
reads) while the others sweep a live cgroup or task population and take
tens to hundreds of µs, scaling with member count — the "µs-scale read span"
the design predicted, just a wider µs range than the tight sampler-stamped
groups. All sampler-stamped and histogram-wave groups (blockio, network,
tcp, `cpu_usage_cpu`/`softirq`/`softirq_time`, `scheduler_runqueue_counters`/
`runqlat`/`running`/`offcpu`, `syscall_counts_counters`,
`cpu_migrations_migrations`, `cpu_tlb_flush_events`) sit at **exactly 0.0**
churn, confirmed again this round.

Cgroup-group churn (8.4–19.3%) is real, not ~0 — plausible for normal cgroup
lifecycle (services/containers starting and stopping), well below the task
group's churn. The task group's **1.0 churn (every tick)** is explained by
measured fork activity on the host: a 10s `/proc/stat` sample showed **~50
forks/sec** against ~1,060 live processes (`ps -e | wc -l`) — more than
enough new/exited tasks per 1s scrape to change membership every tick.
Member counts (398–773) track that same live-process population, order of
magnitude **hundreds, not the 4.2M PID space**.

**3. `drivehealth` — one sweep window, real drive count.** `drivehealth_sweep`
carried a single window per tick in 119/120 ticks (tick 0 has none — before
the sampler's first throttled read completes; expected, not a bug). Median
width **170.3 ms** (max 171.1 ms) — matches the ~176 ms sweep figure from
the earlier per-drive-command journal entry. Members = **161** = 23 gauges
(`drive_temperature`, one per drive) + 138 counters (per-drive threshold/
throttle counters) — the 23 matches this host's real physical disk count
(`lsblk` lists 23 `sdX` disks; four `zd*` zvols are not physical drives and
are excluded). Confirmed **not** the `MAX_DRIVES = 64` array-capacity
constant (`src/agent/samplers/drivehealth/linux/stats.rs:8`) — that bounds
storage, not declared membership, and 161 is real-population-scaled (23
drives × several metrics), not device-count-scaled to 64.

One aside, not part of this check but observed: `gpu_amd_smi_devices` and
`gpu_nvidia_devices` are migrated (non-`/main`) groups with large declared
schemas (416/640 gauge slots) but **zero non-null values and no window on
any tick** — expected on this host, which has no GPU (`gpu_nvidia` fails to
load `libnvidia-ml.so.1`, as in every prior round); the schema is built to
a device-count bound but nothing ever populates it here.

**4. Totals.**

| | wave-1 rerun (60 scrapes, 25 groups) | wave-2 (120 scrapes, 53 groups) | ratio |
|---|---|---|---|
| groups (migrated + default) | 25 + 14 = 39 | 47 + 6 = 53 | |
| entries/tick, median | 2,666.0 | **5,158.5** | 1.94× |
| bytes/tick (v3), median | 335,557 | **640,876** | 1.91× |

Entries and bytes roughly double, as expected from cgroup/task memberships
now being declared instead of suppressed — bounded by live population
(hundreds, not millions), not a blow-up.

v2 bytes/tick spot check (30 scrapes): median **314,648** — within ~2% of
the original wave-1 baseline (321,433.5, 120 scrapes). V2's wire format is
unaffected by internal migration, as expected.

**5. Agent health — no anomalies.**

| | RSS steady-state | CPU% steady-state |
|---|---|---|
| v3 | ~82.6–83.0 MB | 1.8–2.0% |
| v2 | ~68.9 MB | 3.9–4.4% |

v3/v2 steady-state RSS ratio **~1.20×** — actually *tighter* than wave-1's
measured ~1.33×. CPU% reads lower for v3 than v2 at steady state again this
round, consistent with every prior round; still un-repeated `ps`-sampled
noise, not a claim.

**Verdict: the transitional default-group era is not over.** Reader-stamped
groups, the drivehealth sweep, and the wave-2 samplers behave exactly as
designed — windowed, correctly bounded, schema-stable except where real
churn drives it. But four real samplers (`cpu_dtlb`, `cpu_frequency`,
`cpu_l3`, and `cpu_perf`'s base per-CPU counters) are still unmigrated
`/main` groups outside the two accounted-for exceptions
(`cpu_bandwidth`'s two ringbuf gauges, `rezolus_rusage`) — wave 2 is not
"everything" yet.

## Addendum (2026-08-19): gap closed

Re-checked at `ed6a447` (perf-event group closure: `cpu_dtlb`/`cpu_l3`/
`cpu_frequency`/`cpu_branch` sweeps plus `cpu_perf`'s base per-CPU counters).
Residual `/main` groups: exactly the two structural exceptions —
`rezolus_rusage/main` (9) present; `cpu_bandwidth/main` (2, event-driven off
its ringbuf) had no qualifying event in this capture's short window, so it
simply didn't appear, not a regression. All five new sweep groups are
windowed with live-CPU-multiple member counts (96/64/96/64/64 = 32 cores ×
2–3); `cpu_branch_sweep` is windowed but all-null, as its commit predicted
for a host with no branch PMU.
