# Per-cgroup memory accounting

The measurements below describe the original implementation against the 5.x
checkout at `c331a72a`. The PR was subsequently ported to 6.x `main`; see the
final section for the current implementation. The 5.x CPU numbers are not a
benchmark of the port.

Implemented locally: `memory_cgroup` reads cgroup v2's `memory.current`,
`memory.swap.current`, and the byte-valued gauges in `memory.stat`. The default
interval is 60 seconds, configurable in `config/agent.toml`. The sampler is
explicitly opt-in following the overhead/design review below. Cgroup v1 and
the hierarchy root are outside this implementation.

The accounting includes allocations that predate agent startup. Reconstructing
it with allocation probes would require hooks across several hot paths and
still miss existing allocations, so this is a deliberate principle-15 file
interface exception. Discovery and reads run on a blocking worker, throttled
independently of scrapes (principle 17). One acquisition group brackets the
cgroup-major sweep; cached values retain that window between reads.

Three byte-valued gauge families are exposed: `cgroup_memory_usage`,
`cgroup_memory_swap_usage`, and `cgroup_memory_stat{kind=...}`. The `name` label
is the complete path relative to `/sys/fs/cgroup`. Concurrent membership is
capped at 4096 cgroups, using reusable userspace slots rather than indexing by
kernel cgroup ID. Disappearing groups lose their metadata and values; missing
fields/read failures become absent values. The sampler changes no controller
settings or limits.

The snapshot builder now supports metadata-based membership for sampler-stamped
groups independently of reader stamping. This lets each metric use its own
sparse index space (the breakdown flattens cgroup × kind) without emitting the
backing array's entire capacity or refreshing timestamps on cached samples.
Existing reader-stamped groups keep their previous behavior.

Memory plots appear for individual selected cgroups. They are deliberately
excluded from the aggregate panel: memory accounting includes descendants, so
summing ancestors and children is incorrect. Types also overlap (`shmem` is
included in `file`; slab/page tables/stacks in `kernel`); they are separate
lines, not additive stacked buckets. No standalone per-cgroup `Buffers` value
exists. The selector unions memory and CPU names so idle and memory-only
cgroups remain selectable. Analysis sampler attribution includes the new
families.

## Validation and measured cost

- Full binary unit suite: 725 passed, 7 ignored before adding the opt-in
  capacity benchmark. Includes parser/lifecycle tests and sparse snapshot
  membership, schema-cache hits, and cached acquisition-window preservation.
- Dashboard crate: 76 tests passed. Selector tests cover CPU/memory unions and
  memory-only recordings.
- Live memory-only agent, normal scheduler, debug build: 204 cgroups, 6120
  byte-gauge entries, all with values and expected labels. Nine sweeps took
  24,694–42,835 µs (median 31,724 µs). The scrape-path sampler refresh took
  3–225 µs (median 10 µs); the sweep itself runs off that path.
- `capacity_sweep_cost` (explicit ignored test): 4097 filesystem fixtures,
  4096 tracked with a capacity warning. Debug-build first sweep 382,116 µs;
  four subsequent sweeps 183,854–188,024 µs. This validates capacity and bounds
  userspace overhead; ordinary files do **not** reproduce kernel memory-stat
  flush costs. It is not a fleet-scale cgroupfs benchmark.

The measurements justify decoupling collection from high-frequency scrapes;
they do not establish release-build overhead at maximum fleet scale. Hosts
with large hierarchies can configure a longer interval. Reads across files
are not atomic, and the kernel can delay accounting updates.

### Follow-up: whole-agent CPU, RSS, and export cost

Short enabled/disabled comparisons on the same 204-cgroup host, debug binary,
normal scheduling, all other optional samplers disabled, logging at info.
Each process was warmed with 20 scrapes before a ten-second observation.
CPU is the process's user + system time from `/proc/<pid>/stat`, including
worker threads; RSS is from `/proc/<pid>/status`. Requests read the complete
`/metrics/binary` response without requesting compression.

| Scrape rate | Disabled CPU (% of one core) | Enabled CPU | Incremental CPU | Incremental RSS | Added payload per scrape |
|-------------|------------------------------|-------------|-----------------|-----------------|--------------------------|
| 1 Hz | 0.7% | 4.7% | 4.0 percentage points | 13.7 MiB | 961 KiB |
| 10 Hz | 7.0% | 37.7% | 30.7 percentage points | 13.4 MiB | 957 KiB |

The configured collection interval was one second in both cases. This is a
minimum interval measured from sweep completion, with dispatch triggered by
scrapes, so it is not an exact one-Hz background timer (particularly at the
one-Hz scrape rate). Cardinality is 30 series per cgroup: 6120 entries here.
Exporting those entries adds approximately 0.94 MiB per scrape, or 9.3 MiB/s
at ten Hz, even when their acquisition window has not advanced.

These are short debug-build observations, not release-build production
benchmarks or application-throughput measurements. The much larger CPU delta
at ten Hz shows that throttling file reads alone does not make the feature
cheap at high scrape rates; exporting the added series is a substantial cost.

See [metric semantics and queries](../metrics.md#memory_cgroup) and the
[kernel's memory controller interface](https://docs.kernel.org/admin-guide/cgroup-v2.html#memory-interface-files).

## Cadence and default-enablement review

Decision: default to a configurable **60-second minimum interval**, matching
`drivehealth`, and require explicit `enabled = true`. This supersedes the
initial one-second, enabled-by-default implementation. The first scrape
dispatches an initial read immediately; no minute-long startup delay is added.
The user can choose a shorter interval for investigations. Minute sampling is
appropriate for footprint trends and leaks, but misses short spikes and cgroups
that appear and disappear between sweeps. Deletion also becomes visible only
at the next sweep.

| Principle | Existing implementation / evidence | Decision |
|-----------|------------------------------------|----------|
| 1: always-on production | New cardinality adds measurable whole-agent cost | Do not enable automatically fleetwide before export cost is addressed and release-build measurements exist |
| 13: userspace overhead counts | Each snapshot still walks/exports the cgroup series; the TTL caches a whole snapshot, not each slow group's encoded payload across fast snapshots | Use the existing `OPT_IN_SAMPLERS` mechanism, also used by `gpu_amd_pmu`; 60s collection alone is insufficient |
| 15: prefer BPF/perf when equivalent | BPF cgroup CPU/syscall counters can be read via mmap; reconstructing complete pre-existing charged memory from allocation probes is not equivalent | Retain the documented cgroupfs exception for these gauges |
| 16: measure | Live sweep, capacity fixture, and enabled/disabled export measurements are recorded above | Keep debug-build and real-cgroup-scale limitations explicit |
| 17: expensive reads off-cycle | `drivehealth` defaults to 60s and dispatches through `spawn_blocking`; `memory_meminfo`/`memory_vmstat` still read on refresh | Follow `drivehealth` for the hierarchy walk, not the small host-wide procfs readers |
| 18: honest acquisition windows | `drivehealth` retains the sweep window between reads; native `.rez` ingest deduplicates unchanged windows | Keep the cgroup sweep window unchanged between reads; never restamp cached data to make it look fresh |

Follow-up comparison: **60s collection interval, 10 Hz binary scrapes**, same
204-cgroup host and short debug-build protocol as above. Warmup completed the
initial read; the ten-second measurement contained no further sweep:

- Disabled: 0.63 CPU-seconds / 10 wall-seconds = 6.3% of one core.
- Enabled: 3.33 CPU-seconds / 10 wall-seconds = 33.3% of one core.
- Increment: **27.0 percentage points of one core**, **13.2 MiB RSS**, and
  **961 KiB per snapshot** (about 9.4 MiB/s at 10 Hz).

The next performance work is export/schema cost for slow, high-cardinality
groups. Merely caching an encoded payload can save CPU but does not reduce
wire bytes; selective group requests or a negotiated incremental format would
need to preserve independent clients, reconnects, and full-snapshot consumers.
That broader transport change is not part of this sampler's cadence adjustment.

## Release-build correction and cost attribution

The earlier debug CPU measurements must not be treated as production-build
overhead. Built the normal release profile (`cargo build --offline --release
-p rezolus`, optimization and LTO enabled; debug symbols retained). There were
205 cgroups / 6150 memory series in these later runs. Collection was configured
at 60s; warmup completed the first sweep, so no further cgroupfs read occurred
inside either twenty-second CPU measurement. Both used 10 Hz binary scrapes,
normal scheduling and info logging.

| Run | Disabled CPU | Enabled CPU | Increment (% of one core) | Incremental RSS |
|-----|--------------|-------------|---------------------------|-----------------|
| First optimized run | 0.6% | 3.3% | 2.7 percentage points | 11.7 MiB |
| Repeat after timing build completed | 1.45% | 7.35% | 5.9 percentage points | 12.4 MiB |

These short live-host runs are variable, not a fixed 10x speedup claim. The
first ran while another compiler build was active; the repeat ran after it
finished. The host is an Intel i5-13500H with the `powersave` governor, and
neither CPU frequency nor scheduling/core placement was pinned. Report the
observed roughly 3–6% of one core at 10 Hz, not the earlier 27% debug result.
This still is not a benchmark at maximum fleet cgroup count.

Added debug timing lines for snapshot construction, identity validation and
MessagePack encoding. A separate short release run at debug log level measured
30 steady-state scrapes after warmup:

| Phase | Disabled median | Enabled median | Difference of medians |
|-------|-----------------|----------------|-----------------------|
| Label/membership identity validation | 430.5 µs | 4717.5 µs | 4287 µs |
| Whole snapshot construction (includes validation) | 1019.5 µs | 5472.5 µs | 4453 µs |
| MessagePack encoding | 47.5 µs | 1760 µs | 1712.5 µs |

These phase measurements are elapsed times with debug logging, distinct from
the process CPU comparisons with info logging. Most extra construction time
is `fold_group_identities`: enumerating live entries, sorting metadata keys,
and hashing the label strings with FNV-1a-128 to validate the schema cache on
every fresh snapshot. The cache avoids rebuilding/cloning the schema, but
does not avoid this validation pass. Encoding then traverses the full schema
again. The whole-snapshot TTL is 10ms, so it does not help requests 100ms apart.

Decoded the actual MessagePack response to attribute wire size: the memory
group occupies 984,017 bytes, of which **968,456 bytes (98.4%) are schema and
labels**, and only **15,485 bytes are gauge values**. Full cgroup paths, metric
names, units and labels repeat per series. Payload size remains essentially
unchanged by compiler optimization.

Thus minute collection addresses the cgroupfs sweep cost, while the dominant
between-sweep cost is generic snapshot metadata validation and schema encoding.
Possible follow-up work is to reuse validated slow-group snapshots when their
acquisition/membership generation has not advanced, and avoid retransmitting
unchanged schemas through a client-compatible protocol. Simply restamping old
values or omitting schemas unconditionally would violate existing semantics.
The encoded-body cache-hit regression test still passes with the new timing
logs; the logs do not change the snapshot format or cache policy.

## Reuse completed acquisitions and encoded groups

Implemented the CPU optimization above. The cgroup memory acquisition group
now explicitly opts into snapshot reuse. An unchanged completed acquisition
skips both the label/membership identity pass and the metric-value walk. The
snapshot retains its schema and values, and binary exposition reuses the
encoded MessagePack group. Other samplers continue to acquire at their own
cadence; this does not introduce a global sampling rate or change their
snapshot behavior.

The cache uses a revision separate from the acquisition window. Every value,
membership, and metadata mutation in a cacheable group must happen under its
single-writer acquisition guard. Beginning an acquisition marks its revision
in flight; finishing, discarding, or dropping the guard advances the revision.
This matters because a failed acquisition can clear values without publishing
a new window. Reusing solely by window would retain stale values in that case.
The snapshot builder checks the revision before and after reading the group;
if an acquisition overlaps the build, it serves the last completed snapshot
instead. Completed deletions replace the cache with an empty group. The
optimization excludes reader-stamped groups, whose values can change without
a sampler acquisition guard.

Each response remains a complete, independent SnapshotV3, with the same
MessagePack representation as the canonical serializer. The cache preserves
the actual acquisition window and includes the full schema on every scrape.
There is no client state or protocol change. JSON uses the cached observation
but still serializes the response normally. This implements the userspace
overhead and honest-window principles (13 and 18); bandwidth reduction remains
separate work.

Compared release binaries before and after the optimization on the same live
host, with 205 cgroups / 6150 memory series. Both agents and the HTTP driver
were pinned to CPU 0, with normal scheduling, info logging, persistent local
HTTP connections, uncompressed binary responses, and the existing 10ms whole
snapshot TTL. All other configurable samplers were disabled. Collection was
once per minute; after two seconds of warmup, each measurement ran for fifteen
seconds at ten scrapes per second. The measurement verified that the cgroup
window, schema, and values did not change during the run. A second round
reversed the before/after and enabled/disabled order.

| Round | Binary | Disabled CPU | Enabled CPU | Added CPU (% of one core) | Added RSS |
|-------|--------|--------------|-------------|---------------------------|-----------|
| 1 | Before | 1.07% | 6.93% | 5.87 percentage points | 11.81 MiB |
| 1 | After | 0.93% | 2.00% | 1.07 percentage points | 12.66 MiB |
| 2 | Before | 1.27% | 7.20% | 5.93 percentage points | 11.68 MiB |
| 2 | After | 1.33% | 2.00% | 0.67 percentage points | 12.68 MiB |

The incremental CPU reduction was **82–89%** in these short runs. The
additional cache cost, after subtracting each binary's disabled baseline,
was approximately **0.9–1.0 MiB RSS**. This is a steady-state export benchmark,
not a measurement of the once-per-minute sweep or maximum-capacity behavior.
CPU frequency and other host workloads remain uncontrolled.

Separate debug-log release runs measured thirty steady-state scrapes each:

| Phase | Before median | After median |
|-------|---------------|--------------|
| Label/membership identity validation | 4967.5 µs | 493.5 µs |
| Whole snapshot construction (includes validation) | 5637.5 µs | 1301.5 µs |
| MessagePack encoding | 1947 µs | 384.5 µs |

These timings cover the whole snapshot, including groups that do not opt into
the cache. Encoding still allocates the response and copies the cached bytes.
The enabled response remains approximately 990 KB: the cgroup group still adds
about 0.94 MiB per scrape, or 9.4 MiB/s at ten Hz.

Validation: 728 root unit tests passed, with eight ignored; Clippy and formatting
checks passed. Added regressions cover acquisition completion/discard/drop,
in-flight reads, changed values and labels, deletion, a revision change during
snapshot construction, and independent clients. Binary output is checked
against canonical serialization, including mixed cached/uncached groups,
MessagePack array-header boundaries, and encoding an older snapshot after the
cache advances.

## Port to the current 6.x pipeline

The PR targets `main` at `6335eb0b`, where registry walks and schema caches
belong to metriken-exposition's `GroupBuilder`, and the recorder consumes the
native stream. Reuse is now a small wrapper around that builder rather than a
change to the old local snapshot walker. Wide and long layouts have separate
completed-acquisition caches. The router skips unchanged or in-flight groups;
the wrapper appends their last completed observation with its original window.
The full binary endpoint retains the encoded-group optimization and canonical
MessagePack compatibility. Native streaming retains its existing schema and
unchanged-window suppression.

Cacheable acquisitions hold a write guard on their blocking worker. Snapshot
builds only try a read guard: an active sweep immediately falls back to cached
data, while a new sweep may wait briefly for an existing snapshot copy. This
prevents a racing sweep from poisoning the library's persistent schema cache
or stream occupant identities. The revision still advances on failed reads
and guard drop, so an unchanged window cannot conceal cleared values. Other
samplers do not acquire this lock.

Memory kinds are now separate gauge columns over the same cgroup slot. Every
column carries its `kind` statically and shares the cgroup's `name` labels.
The long stream joins columns by slot, so flattening cgroup and kind into one
slot index, as the original wide-only implementation did, would mislabel
values. The external metric names and queries are unchanged. The Cgroups
section template and its Rust reference generator both include the memory
plots; discovery includes idle, memory-only cgroups.

The historical release benchmark above is retained for provenance. It does
not establish the CPU saving or memory footprint on the new 6.x pipeline,
which already avoids repeated string hashing and streams compact long rows.

Port validation: the root binary's full unit suite passed (1185 tests, 12
ignored), including localhost stream and recorder tests. After adding the
real-sampler long-layout assertion, the agent suite passed again (414 tests,
eight ignored, network stream tests excluded from this targeted rerun).
The dashboard suite passed (246 tests, four ignored), including template/
generator parity and updated Cgroups golden files. The discovery and selector
JavaScript tests, formatting and whitespace checks passed. Clippy completed
with one pre-existing `manual_range_contains` warning in `src/mcp/link.rs`;
strict `-D warnings` is blocked by that unchanged upstream warning.

## Local review follow-up

Commit `f0c3c588` addresses the correctness and local efficiency findings
from the review of `c38166f8`:

- Memory's `name` now uses the same shared formatter as BPF cgroup metrics,
  including the last-three-component abbreviation, component length limit,
  and systemd escape decoding. A separate `path` label retains the complete
  cgroupfs path and distinguishes abbreviated-name collisions.
- Sweeps stage a bounded set of readings before publishing. Failed discovery
  or total read failure preserves the previous values, membership and window;
  partial failures in a successful sweep remain absent values. Empty sweeps
  still publish deletion. The production sweep helper is exercised directly
  by the failure/recovery tests.
- The blocking worker catches and logs panics, recovers poisoned state, and
  releases an RAII in-flight lease. Retry respects the configured interval.
- Cached groups use the explicit `acquire_blocking()` API. Ordinary
  `acquire()` stays nonblocking and rejects cached groups with an actionable
  error before reaching Tokio's blocking lock.
- A shared test mutex serializes every test that mutates the memory metrics,
  including the ignored capacity benchmark.
- Routing checks the group's cache opt-in before touching the skip mutex, so
  ordinary groups do not pay that extra lock. The skip set is moved into and
  back out of the router, rather than cloned. Encoded-byte storage exists
  only in the wide cache; the long cache uses a zero-sized encoding type.

The suggestion to expose a skip-set argument in metriken-exposition is
**deferred**. Its current public builder API does not expose that argument;
changing it requires a separately reviewed dependency change and release.
The local adapter is restricted to opted-in groups and holds its skip set
constant across both registry passes. Revisit it when the dependency exposes
an exclusion API. The existing PR description already scoped encoded-byte
reuse to full binary responses; long-format streaming reuses observations,
not MessagePack bodies.

The follow-up also merges `main` at `bffcf842`, preserving the new default-on
sensor configuration. The merged metriken release adds gauge histograms;
encoded-group matching now checks those values too, and the canonical-byte
regression primes the old cached body before changing only a gauge histogram.

Validation of the review fixes: 1189 root tests passed before the main merge,
and all six cgroup tests passed with the ignored capacity test enabled on
four test threads. After merging main and adapting to gauge histograms, the
root suite passed all 1130 tests (12 ignored), and the dashboard suite passed
249 (four ignored). The changed test total reflects upstream's recording-tool
refactor. Cgroup JavaScript tests, formatting and PR-diff whitespace checks
passed. Clippy completed with the same pre-existing warning in `src/mcp/link.rs`.
