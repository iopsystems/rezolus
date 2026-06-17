# Proposal: Per-cgroup attribution for block I/O and network samplers

## Motivation

Rezolus already attributes CPU, scheduler, syscall, and TLB activity per cgroup
(see `cgroup_cpu_usage` in `cpu/usage`). On a hypervisor where each guest/tenant
is a cgroup (e.g. libvirt `machine.slice/machine-qemu-…`), that gives per-tenant
CPU/scheduling/syscall visibility from the host.

The block I/O and network samplers do **not** yet break down by cgroup —
`blockio_*` is labeled only by `op`, `network_traffic` only by `direction`. This
proposal adds per-cgroup variants so an operator can see per-tenant disk and
network behavior from the host, consistent with the existing per-cgroup CPU
metrics.

This is the enhancement called out as a gap in
`docs/gpu_hypervisor_tenant_insights.md`.

## What "good" looks like

New metric groups, mirroring the `cgroup_cpu_usage` naming/shape:

| Metric | Type | Key | Labels |
|--------|------|-----|--------|
| `cgroup_blockio_operations` | counter | `MAX_CGROUPS` | `op={read,write,flush,discard}`, `name=<cgroup path>` |
| `cgroup_blockio_bytes` | counter | `MAX_CGROUPS` | `op={read,write,…}`, `name` |
| `cgroup_network_bytes` | counter | `MAX_CGROUPS` | `direction={receive,transmit}`, `name` |
| `cgroup_network_packets` | counter | `MAX_CGROUPS` | `direction={receive,transmit}`, `name` |

Per-cgroup **histograms** (blockio size distribution) are intentionally **out of
scope for v1** — see "Histograms" below.

## The hard part: whose cgroup? (attribution context)

This is the crux, and it's why the existing hooks can't just call
`bpf_get_current_cgroup_id()`. Both samplers fire in contexts where `current` is
**not** the tenant that owns the I/O.

### Block I/O — completion runs in IRQ/softirq context

`block_rq_complete` fires on the completing CPU, long after submission;
`current` is whatever happened to be running. The issuing cgroup must come from
the request itself. The kernel carries it on the bio:

```
struct request *rq → rq->bio (struct bio*) → bio->bi_blkg (struct blkcg_gq*)
                   → blkg->blkcg (struct blkcg*) → blkcg->css.id
```

Both `bi_blkg` and `struct blkcg_gq`/`struct blkcg` are present in the checked-in
`vmlinux.h` (confirmed at `x86_64/vmlinux.h:10817,25069,25103`), so this is a
CO-RE read with no kernel patch (principle 2).

**Caveats to handle:**
- `rq->bio` can be NULL (e.g. flushes). Guard and skip — these already contribute
  no bytes.
- `bi_blkg` requires `CONFIG_BLK_CGROUP`. If the chain reads NULL, fall back to
  cgroup id `0` (root) rather than dropping the count, so totals still reconcile.
- The `css.id` here is the **blkio** controller's css, a *different* id space than
  the **cpu** controller's `sched_task_group.css.id` used by `cpu/usage`. That's
  fine — each sampler resolves its own id→name mapping independently via the
  kernfs node, and every metric carries its own `name` label. We do **not** try
  to share cgroup ids across samplers.

### Network — RX/TX run in softirq/NAPI context

`netif_receive_skb` (RX) and `net_dev_start_xmit` (TX) also run outside the
owning task. The cgroup must come from the socket on the skb:

```
struct sk_buff *skb → skb->sk (struct sock*) → sk->sk_cgrp_data → cgroup id
```

- **TX** (`net_dev_start_xmit`): `skb->sk` is normally populated for locally
  originated traffic, so socket-based attribution works.
- **RX** (`netif_receive_skb`): `skb->sk` is typically **NULL** at this point —
  the packet hasn't been demuxed to a socket yet. So RX attribution at this hook
  is unreliable.

**Two options for network; this needs a decision (see "Open questions"):**

1. **Stay at the device hooks, TX-attributed only.** Ship `cgroup_network_bytes`
   /`_packets` for `direction=transmit` now; leave RX as host-aggregate. Lowest
   overhead, no new probes, but asymmetric.
2. **Attribute at the socket layer instead.** Hook `sock_sendmsg`/`tcp_sendmsg`
   (TX) and `tcp_cleanup_rbuf`/`tcp_recvmsg` (RX), where `current`/`sk` is valid.
   This gives symmetric RX+TX but **overlaps the existing `tcp/*` samplers** —
   principle 11 says consolidate rather than add a second attach to a hook
   another sampler already covers. That makes it a larger, cross-sampler change
   (fold cgroup attribution into the tcp samplers) and it only covers TCP, not
   all of `network_traffic`.

My recommendation: **ship blockio first** (cleaner story), and do network as a
**TX-first** increment (option 1) with RX deferred behind the socket-layer
consolidation discussion.

## Design (mirrors `cpu/usage`)

The plumbing is already a well-worn path in this codebase; we reuse it wholesale
(principle 12 — shared headers, no reinvention).

### BPF side (per sampler)

Add to each target `mod.bpf.c`:

1. **cgroup metadata channel** (copy from `cpu/usage`):
   - `cgroup_info` ringbuf (`RINGBUF_CAPACITY`) — rare new-cgroup metadata only,
     never per-event (principle 3).
   - `cgroup_serial_numbers` array (`MAX_CGROUPS`, `BPF_F_MMAPABLE`).
2. **per-cgroup counter arrays** — `BPF_MAP_TYPE_ARRAY`, `BPF_F_MMAPABLE`,
   `max_entries = MAX_CGROUPS`, keyed by the bounded cgroup id (principle 5:
   array over hashmap for a bounded-integer key). One map per (metric, op/dir):
   e.g. `cgroup_read_ops`, `cgroup_write_ops`, …, or a single packed group of
   width `MAX_CGROUPS * GROUP_WIDTH` — match whichever the existing per-cgroup
   metrics use with `packed_counters`.
3. In the handler, after the existing global counter update, derive `cgroup_id`
   via the chain above, bounds-check (`if (id >= MAX_CGROUPS) return 0;`), call a
   new shared helper to emit metadata on first sight, then
   `array_add(&cgroup_*, id, delta)` with relaxed atomics.

New shared helper in `src/agent/bpf/cgroup.h` (alongside `handle_new_cgroup` /
`handle_new_cgroup_from_css`):

```c
// Populate cgroup_info from a blkcg_gq (block I/O completion path, where
// `current` is not the issuing task).
static __always_inline int handle_new_cgroup_from_blkg(
    struct blkcg_gq *blkg, void *serials, void *ringbuf);
```

This factors the css→name/level/parent walk that already exists in
`handle_new_cgroup_from_css`; the only difference is where the `css` comes from
(`blkg->blkcg->css` vs `task->sched_task_group->css`). Refactor the common tail
into one `__always_inline` body to avoid a third copy of the kernfs walk.

### Userspace side (per sampler `mod.rs` + `stats.rs`)

Copy the `cpu/usage` pattern verbatim:
- `unsafe impl plain::Plain for bpf::types::cgroup_info {}` + `impl_cgroup_info!`.
- `CGROUP_METRICS` slice + `handle_cgroup_info` → `process_cgroup_info`.
- Register maps in `SkelExt::map`, wire `.packed_counters("cgroup_…", &…)` and
  `.ringbuf_handler("cgroup_info", handle_cgroup_info)` in `init`.
- `stats.rs`: add `CounterGroup::new(MAX_CGROUPS)` metrics named
  `cgroup_blockio_*` / `cgroup_network_*`, following the `CGROUP_CPU_USAGE_*`
  block.

No new userspace refresh cost beyond O(active cgroups) (principle 13): the reader
is mmap-direct over the `MAX_CGROUPS` array, same as `cgroup_cpu_usage`.

## Histograms (deferred)

Per-cgroup blockio **size distributions** would mean a `MAX_CGROUPS × 496-bucket`
H2 histogram per op — ~16 MB per op, ~64 MB for four ops, per sampler. That
violates the bounded-memory discipline (principles 8, 13). v1 ships per-cgroup
**counters only** (ops + bytes). If distributions are needed later, gate them
behind config and/or a sparse representation — separate proposal.

## Overhead assessment, baselined against `cpu/usage`'s cgroup tax (principle 1)

The right baseline is the **existing per-cgroup CPU sampler** (`cgroup_cpu_usage`),
which is already deployed always-on fleetwide. The work this proposal adds to the
blockio/network hooks is the *same cgroup-attribution "tax"* that `cpu/usage`
already pays on top of its base accounting. Decomposed, that tax is:

| Step (per event) | Cost | `cpu/usage` today | This proposal |
|---|---|---|---|
| Resolve cgroup id (pointer chase to a `css.id`) | a few `BPF_CORE_READ`s | `task → sched_task_group → css.id` (**2 derefs**) | blockio: `rq → bio → bi_blkg → blkcg → css.id` (**4 derefs**); network: `skb → sk → sk_cgrp_data` (**2–3 derefs**) |
| New-cgroup gate (`handle_new_cgroup*`) | 1 array lookup + serial compare; ringbuf **only on first sighting** | identical (shared `cgroup.h` helper) | identical (shared helper) |
| Accumulate | 1–2 `array_add` (relaxed atomic) into a `MAX_CGROUPS` array | 2 (`cgroup_user`,`cgroup_system`) | 2 (ops + bytes / packets + bytes) |

So the **marginal per-event cost is essentially the cpu baseline's cgroup
portion**, with one wrinkle: blockio's id chain is ~2 dereferences longer (each a
bounded `bpf_probe_read_kernel`), so its tax is modestly higher per event than
cpu's; network's is comparable to cpu's (plus one NULL-`sk` branch on RX). All
O(1), no refused helpers, no new probe (we extend existing attaches).

**The axis that actually differs is event rate, not per-event cost.** The cgroup
tax is constant per event; aggregate overhead = tax × hook firing rate. Ordering
the three hosts hooks by worst-case rate:

| Sampler | Host hook | Event-rate ceiling |
|---|---|---|
| `cpu/usage` (baseline) | `cpuacct_account_field` | ~per-task-per-tick — modest, scales with runnable tasks |
| `blockio/requests` | `block_rq_complete` | **millions of IOPS** on NVMe under fio |
| `network/traffic` | `netif_receive_skb` / `net_dev_start_xmit` | **Mpps** at line rate — the highest |

This is exactly principle 1's named ceiling. The conclusion: **per-event, this is
no worse than a sampler we already run fleetwide; the thing to respect is that the
completion/packet hooks can fire far more often than tick accounting, so the same
tax multiplies further on extreme IOPS/pps workloads.** That is an argument about
*which hook to attribute at* (and how often it fires), not about the cgroup
machinery itself.

**Memory & userspace:** identical shape to `cpu/usage` — `MAX_CGROUPS (4096) ×
u64` per counter map (cpu's cgroup maps are 2 × 4096 × 8 = 64 KB; blockio ~256 KB,
network ~128 KB), plus the shared serial-number array and ringbuf. Userspace
refresh is the same mmap-direct, O(active cgroups) read the cpu cgroup sampler
already does (principle 13). Negligible and already-proven.

**Consolidation note:** `block_rq_complete` is already shared logic between
`blockio/latency` and `blockio/requests` (flagged in principles "Known drift").
Add the cgroup counters in `blockio/requests` only; don't add a second attach.

### Measure it, don't guess

Every sampler already exports `rezolus_bpf_run_time` and `rezolus_bpf_run_count`
(see `BPF_RUN_TIME`/`BPF_RUN_COUNT`). The honest way to size this is empirical:
take `run_time / run_count` (mean ns per probe invocation) for `cpu_usage` as the
established baseline, then compare the same ratio for `blockio_requests` /
`network_traffic` **before and after** adding the cgroup path, under a load
generator that drives the hook hard (fio for blockio, a packet generator for
network). The delta in mean-ns-per-event is the cgroup tax; multiply by expected
fleet event rates to bound aggregate cost. This avoids hand-waved cycle counts and
uses instrumentation that already ships.

## File-by-file change list

**Block I/O (do first):**
- `src/agent/bpf/cgroup.h` — add `handle_new_cgroup_from_blkg` + refactor shared
  tail.
- `src/agent/samplers/blockio/linux/requests/mod.bpf.c` — cgroup maps + per-cgroup
  `array_add` in `handle_block_rq_complete`.
- `src/agent/samplers/blockio/linux/requests/mod.rs` — cgroup_info plumbing, map
  registration, `packed_counters`/`ringbuf_handler`.
- `src/agent/samplers/blockio/linux/requests/stats.rs` — `CGROUP_BLOCKIO_*`
  metric definitions.

**Network (TX-first increment):**
- `src/agent/samplers/network/linux/traffic/mod.bpf.c` — cgroup maps + socket→
  cgroup read on the TX path.
- `…/traffic/mod.rs`, `…/traffic/stats.rs` — same plumbing as above.

**Docs:**
- `docs/metrics.md` — document the new `cgroup_blockio_*` / `cgroup_network_*`
  metrics.
- `docs/principles.md` — tick the relevant "Known drift" note if applicable.

## Testing

- `cargo build` (triggers `build.rs` BPF compilation) on both `x86_64` and
  `aarch64` headers.
- `cargo clippy`, `cargo xtask fmt`.
- Manual: run the agent under load that exercises a known cgroup (e.g. a
  container doing `fio` and `iperf`), confirm `cgroup_blockio_*` /
  `cgroup_network_*` attribute to the expected `name=` path and that summed
  per-cgroup values reconcile against the existing global `blockio_*` /
  `network_traffic` totals.
- Verify graceful behavior when `CONFIG_BLK_CGROUP` is off (falls back to root).
- Verify the `raw_tp` twins (CO-RE-only kernels, no in-kernel BTF) still load.

## Phasing

1. **PR 1 — blockio counters per cgroup.** Self-contained, clean attribution
   model, immediately useful for per-tenant disk.
2. **PR 2 — network TX per cgroup** at the device hook.
3. **PR 3 (discussion-gated) — network RX** via socket-layer consolidation with
   the `tcp/*` samplers, if symmetric RX/TX attribution is wanted.

## Open questions (need a decision before coding network)

1. **Network RX attribution:** accept TX-only at the device hook (option 1), or
   take on the socket-layer consolidation with the tcp samplers (option 2)?
2. **Counter layout:** `packed_counters` keyed by cgroup id (matches
   `cgroup_cpu_usage`) — confirm we want the same `PackedCounters` strategy here
   (principle 7: high-cardinality keyed counters, one natural writer per cgroup).
3. **Metric naming:** `cgroup_blockio_operations`/`cgroup_blockio_bytes` vs.
   reusing `blockio_operations` with an added `cgroup` label. The existing
   convention is a distinct `cgroup_`-prefixed metric (`cgroup_cpu_usage`), so I'd
   follow that.
