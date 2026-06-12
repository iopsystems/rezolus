# Reading AMD GPU device counters without the HIP runtime

This note explains how Rezolus can read **device-wide AMD GPU hardware
performance counters** (the PMCs that `rocprofv3` programs) from the agent
process **without initializing the HIP runtime** and without instrumenting the
GPU workload. It is the design basis for the planned PMC layer of the
`gpu_amd_smi` sampler (`src/agent/samplers/gpu/linux/amd/`).

The telemetry layer (power, temperature, clocks, VRAM, SMI-level utilization)
is a separate concern handled by `rocm_smi.rs` and described in
[`gpu_amd_smi.md`](gpu_amd_smi.md); this note is only about the hardware counter
layer.

## The right primitive: rocprofiler-sdk device counting service

`rocprofv3` has no device-wide CLI mode — its `--pmc` flag is per-kernel
*dispatch* counting and requires launching the target application. For an
always-on fleet agent we instead use the **device counting service**
(a.k.a. agent profile counting) from the rocprofiler-sdk **C API**:

```
rocprofiler_configure_device_counting_service(ctx, buffer, agent, set_profile_cb, ...)
```

Properties that make it the correct fit:

- **Device-wide.** It aggregates counter activity across the whole GPU, from
  *all* processes, not a single kernel or a single client process.
- **No kernel serialization.** Unlike dispatch counting it does not serialize
  kernels and cannot deadlock co-dependent kernels.
- **Non-invasive.** It does not need to hook into the profiled application's
  HIP/HSA runtime. The agent samples the device; the workload runs untouched
  in other processes.

### Core sampling loop

```
create_context
create_buffer
configure_device_counting_service(... set_profile callback ...)
create_counter_config(agent, counter_ids, n, &profile)   // resolve names -> ids
loop every interval:
    start_context
    (wait the sample window)
    rocprofiler_sample_device_counting_service(ctx, ..., out, &out_size)
    stop_context
```

Counter names are resolved to IDs per agent via
`rocprofiler_iterate_agent_supported_counters` +
`rocprofiler_query_counter_info`.

## Why HIP is not required (but HSA init is)

HIP (`libamdhip64`) is the high-level layer: it creates compute queues, loads
code objects, manages streams and device allocations. **None of that is needed
to read device counters.** HIP internally sits on top of the **HSA runtime
(ROCr, `libhsa-runtime64`)**, and it is only HSA that the counter path needs.

What `hsa_init()` does (and all the counter path needs):

- Opens the kernel driver (`/dev/kfd`, the amdgpu/KFD interface).
- Discovers GPU topology from `/sys/class/kfd/.../topology/nodes/`.
- Registers the **agents** (CPU, GPU, ...). This is the agent list that
  `rocprofiler_query_available_agents` returns.
- Sets up the HSA API dispatch table.

It does **not** create queues, load kernels, or allocate device buffers. It is
a much lighter dependency than HIP.

### HSA init is also what *arms* rocprofiler

rocprofiler-sdk activates via `rocprofiler-register` interception hooks that
fire **when the HSA runtime initializes in the same process**. Empirically
verified on an MI-class / RDNA host (gfx1201, ROCm 7.2.1):

| Host process | GPU load source | Result |
| --- | --- | --- |
| plain `sleep` (no HSA, no HIP) | — | `rocprofiler_configure` **never called**; nothing sampled |
| calls only `hsa_init()`, then waits | none | samples fine; counter sums read `0` (idle GPU) |
| calls only `hsa_init()`, then waits | a **separate** HIP process | counter sums track the other process's work (e.g. `SQ_WAVES` ≈ 145k under load) |

So the minimum requirement for the agent process is:

1. Link/`dlopen` `libhsa-runtime64` and call `hsa_init()` **once** at startup
   (check `HSA_STATUS_SUCCESS`). Keep the runtime alive for the sampler's
   lifetime; call `hsa_shut_down()` on teardown to balance the refcount.
2. Register the rocprofiler tool (`rocprofiler_configure` entry point + the
   device-counting client) so the register hooks pick it up during HSA load.
3. Run the device-counting sample loop on each `refresh()`.

No `libamdhip64`, no kernels, no compute stream.

## Privileges

Device-wide counters require the **`CAP_PERFMON`** capability — **full root is
not required**. Without it, the driver logs
`Device could not be locked for profiling (capability SYS_PERFMON)` and counter
values read back as `0`/inaccurate. Granting `CAP_PERFMON` to the agent (file
capability or ambient cap) is sufficient, consistent with how Rezolus already
runs its eBPF samplers under elevated capability rather than full root.

## Counter values are per hardware instance

Records come back **per hardware instance**, broken out by dimensions
(`DIMENSION_SHADER_ENGINE` × `DIMENSION_SHADER_ARRAY` × `DIMENSION_WGP`, etc).
The sampler must **sum across dimensions** to produce a single device-level
value per metric. (For NVIDIA, the equivalent SM-wide aggregation is done by
the GPM library; here we do it ourselves.)

## Counter naming is architecture-specific

Counter names differ between CDNA (MI300, gfx942) and RDNA (gfx12xx). List the
counters available on the actual hardware with:

```
rocprofv3-avail list --pmc        # per-agent counter names
rocprofv3-avail info --pmc        # names + descriptions + dimensions + block
```

Verified available and functional on gfx1201 (RDNA4):
`GRBM_GUI_ACTIVE`, `GRBM_COUNT`, `GPUBusy`, `SQ_WAVES`, `VALUBusy`,
`SQ_INSTS_VALU`, `MemUnitBusy`, `OccupancyPercent`, `L2CacheHit`,
`GL2C_HIT` / `GL2C_MISS`, `GL2C_EA_RDREQ` / `GL2C_EA_WRREQ` (bandwidth),
`FetchSize` / `WriteSize`. Note RDNA L2 counters are `GL2C_*`, **not** CDNA's
`TCC_*`.

Constraint: a profile must fit the per-block physical counter slot budget in a
**single pass** (e.g. GRBM has 2, SQ has 8). Pick one fixed single-pass set
rather than time-multiplexing groups, for consistent readings — consistent
with the always-on, in-place-aggregation discipline in `docs/principles.md`.

## Build / linking notes

- The agent must compile on hosts **without ROCm installed**. Follow the
  existing `rocm_smi.rs` pattern: load the libraries at runtime with `dlopen`
  (resolving the rocprofiler-sdk + HSA symbols dynamically) rather than linking
  at build time, and return `Ok(None)` from `init()` when ROCm / an AMD GPU is
  absent so the sampler reports as disabled, not failed.
- If any rocprofiler-sdk headers are used for bindgen, note they transitively
  include HIP headers and need `-D__HIP_PLATFORM_AMD__` defined even though no
  HIP runtime is linked or used.
- Relevant shared libraries on a ROCm host: `librocprofiler-sdk.so`,
  `librocprofiler-register.so`, `libhsa-runtime64.so`. The reference sample is
  shipped at
  `/opt/rocm/share/rocprofiler-sdk/samples/counter_collection/device_counting_sync_client.cpp`.

## Summary

To read AMD GPU device counters without HIP:

1. Use the rocprofiler-sdk **device counting service** (C API), not `rocprofv3`
   `--pmc` and not dispatch counting.
2. In the agent process, initialize **only the HSA runtime** (`hsa_init()`) —
   this opens KFD, enumerates GPU agents, and triggers rocprofiler tool
   activation. HIP is not needed.
3. Register the rocprofiler tool, build a single-pass counter profile (names
   resolved per agent), then `start_context` → `sample_device_counting_service`
   → `stop_context` each interval.
4. Run with **`CAP_PERFMON`** (not full root).
5. Sum counter records across hardware-instance dimensions to get device-level
   values; use architecture-correct counter names (verify with
   `rocprofv3-avail`).

---

# What we learned about AMD device-level PMU (empirical findings)

Everything below was verified on a real machine, not inferred from docs:
**AMD Radeon AI PRO R9700 (gfx1201 / RDNA4)**, ROCm 7.2.1,
rocprofiler-sdk v1.1.0, Ubuntu 24.04 / kernel 6.17. Where it matters, results
were cross-checked against `rocm-smi` and a live `vllm` inference workload.
**These findings are RDNA4-specific; CDNA/MI300 must be re-tested — see caveats.**

## 1. Device-level counting works and is decoupled from the workload

The rocprofiler-sdk **device counting service** reads counters for the *whole
GPU over a wall-clock window*, aggregating activity from **all processes**. Our
sampler process ran **zero GPU kernels** (only `hsa_init()`) yet correctly read
counters driven by a *separate* process's kernels. Verified against a live
vLLM server: device counters tracked real inference activity
(`GPUBusy`=100%, `SQ_INSTS_VALU`≈1.3B/window, read:write request ratio ≈350:1
— the read-heavy signature of weight streaming), and agreed with `rocm-smi`'s
independent GPU-use / memory-activity telemetry.

This is exactly the model an always-on agent needs: sample the whole GPU on an
interval, see every workload, instrument none of them.

## 2. HSA is required in-process; HIP is not

Counter collection needs the **HSA runtime** (`libhsa-runtime64`) initialized in
the agent process — a single `hsa_init()` call. It does *not* need HIP
(`libamdhip64`), kernels, queues, or device buffers. `hsa_init()` opens
`/dev/kfd`, enumerates GPU agents, and (critically) triggers rocprofiler tool
activation via the `rocprofiler-register` hooks. With no HSA init at all,
`rocprofiler_configure` is never even called.

## 3. **RDNA requires a stable power state — this is the biggest gotcha**

On gfx10/11/12 (all RDNA, incl. Radeon 7000/9000), most counters **silently
return 0** unless the GPU is pinned to a stable power state. This is documented
AMD behavior, not a bug. At the default perf level (`auto`/`manual`) the
*globally-accumulated* counters (GRBM clocks, `SQ_WAVES`, `SQ_BUSY_CYCLES`)
still read correctly, but every *per-SIMD / windowed* counter
(VALU, occupancy, memory bandwidth, L2, TA, TCP) reads 0.

Fix — set before reading, restore after:
```
sudo amd-smi set -g <N> -l stable_std   # before profiling
sudo amd-smi set -g <N> -l auto         # after
```
(equivalently the `power_dpm_force_performance_level` sysfs node.)

Proof of causation: with `stable_std`, a full 84-counter scan went from
**15 → 65 working**; `SQ_INSTS_VALU` read 1.28B. Toggling back to `auto`,
`SQ_INSTS_VALU` dropped to 0 while `SQ_WAVES` stayed nonzero.

**Operational consequence for an always-on agent:** `stable_std` pins clocks to
a fixed profiling state, changing the GPU's performance characteristics. Holding
it continuously to keep counters live is a real trade-off (it briefly forced a
live vLLM GPU to a fixed clock during testing). The sampler must either toggle
it per-sample, hold it for the agent's lifetime, or fall back to SMI-only
metrics when it can't/shouldn't pin clocks. **Decision still open.**

## 4. This requirement is mode-independent

The earlier hypothesis that the zero counters were "dispatch/kernel-mode only"
was **wrong**. Tested via the dispatch (per-kernel) counting C API too: the same
counters that were zero device-wide were also zero per-kernel — because *both*
paths were missing the stable power state, not because of the mode. Once
`stable_std` is set, both device-level and kernel-level counting work.
(Separately: the `rocprofv3 --pmc` CLI itself crashes on this box+ROCm, but the
rocprofiler-sdk C API — both device and dispatch — works fine.)

## 5. Counters are per-hardware-instance; sum across dimensions

Records come back per HW instance with dimensions
(`DIMENSION_SHADER_ENGINE` × `SHADER_ARRAY` × `WGP`, etc.). A device-level value
is the **sum across all instances** of a counter. (NVIDIA's GPM library does
this SM-wide aggregation internally; here we do it ourselves.)

## 6. Physical PMU counter budget is per-block (single-pass limit)

There is no single global counter count. Each hardware block has a fixed number
of physical counter slots — exceed it in one profile and config/sample fails
("Request exceeds the capabilities of the hardware to collect"). Measured on
gfx1201:

| Block | Slots | Covers |
| --- | --- | --- |
| SQ | 8 | waves, VALU, instruction mix, occupancy |
| GL2C | 4 | L2 cache hit/miss, **memory bandwidth** (EA req) |
| SQC | ≥5 | instruction cache |
| TA | 2 | texture/memory-unit busy |
| TCP | 2 | L1 vector cache |
| GRBM | 2 | GPU busy % |

Blocks are independent, so ≈23 counters can be live in one pass. Design a single
fixed counter set that fits these per-block limits rather than time-multiplexing
counter groups (which breaks window consistency — bad for an always-on agent).

## 7. Privilege: `CAP_PERFMON`, not root

Device-wide counting needs the `CAP_PERFMON` capability. Without it the driver
logs *"Device could not be locked for profiling (SYS_PERFMON)"* and values read
0/inaccurate. Full root is not required — consistent with how Rezolus already
runs eBPF samplers under elevated capability.

## 8. Naming is architecture-specific

RDNA uses `GL2C_*` for L2; CDNA/MI300 uses `TCC_*`. Always resolve names against
the live agent (`rocprofiler_iterate_agent_supported_counters`) and verify with
`rocprofv3-avail list --pmc`. The generic `derived_counters.xml` expressions
reference `TCC_*` but resolve to `GL2C_*` on RDNA.

## What this means for the Rezolus AMD PMC sampler

- ✅ On RDNA4 (with `stable_std`) we *can* get device-wide GPU busy %, VALU
  utilization, occupancy, L2 hit rate, and memory bandwidth — the metrics that
  matter — with no workload instrumentation.
- ⚠️ The `stable_std` clock-pinning trade-off is the key unresolved design
  question for continuous monitoring.
- 🔬 All numbers above are RDNA4. **MI300/CDNA is the likely datacenter target
  and must be re-characterized**: it may not need the stable power state, has
  different counter blocks (TCC/EA/MFMA/HBM) and likely larger per-block slot
  budgets. Re-run the same scans (counter availability, power-state dependence,
  per-block slot probe) on an MI300 before finalizing the sampler.
