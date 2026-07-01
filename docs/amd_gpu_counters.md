# AMD GPU telemetry in Rezolus: SMI and PMU samplers

Rezolus collects AMD GPU telemetry through **two complementary samplers**, both
under `src/agent/samplers/gpu/linux/amd/`:

| Sampler | Name | Source | What it measures |
|---|---|---|---|
| **SMI** | `gpu_amd_smi` | ROCm SMI library (`librocm_smi64.so`) | Board-level telemetry: power, temperature, clocks, VRAM, fan, SMI-level utilization. Metric prefix `gpu_*`. |
| **PMU** | `gpu_amd_pmu` | rocprofiler-sdk device counting service | Hardware performance counters: clocks, waves, instruction mix, caches, memory traffic. Metric prefix `gpmu_*`. |

They answer different questions — *"is the board healthy / how hot / how much
power"* (SMI) vs *"what is the shader array actually doing"* (PMU) — and are kept
separate because they have very different cost, privilege, and reliability
profiles. Both load their vendor library at runtime via `dlopen`, so the agent
still **compiles and runs on hosts without ROCm or an AMD GPU** (each sampler
returns `Ok(None)` at init and reports as disabled, not failed).

---

## Design summary

### Why two samplers, not one

The SMI library and the rocprofiler-sdk are entirely different stacks with
different constraints:

- **SMI is cheap, safe, and always-on.** `librocm_smi64.so` reads sysfs/ioctl
  board sensors. No special privilege beyond device access, no GPU lock, no
  workload interference, and the values are always valid regardless of GPU
  power state. This is the baseline GPU telemetry every fleet host should have.
- **PMU is powerful but expensive and fragile.** Reading hardware performance
  counters requires `CAP_PERFMON`, takes an **exclusive per-GPU device
  profiling lock**, and on RDNA only reads correct values under a pinned power
  state. It is opt-in and intended for hosts where deeper GPU profiling is
  worth the cost.

Folding both into one sampler would force the cheap, always-safe SMI metrics to
inherit the PMU's privilege and reliability constraints. Keeping them separate
lets an operator run SMI everywhere and enable PMU only where wanted.

### The SMI sampler (`gpu_amd_smi`)

- Loads `librocm_smi64.so` via `dlopen`, calls `rsmi_init(0)`, enumerates
  devices with `rsmi_num_monitor_devices`, and reads each board sensor.
- `refresh()` is a **direct synchronous read** of the current sensor values into
  gauge metrics keyed by GPU `id` — no background thread, no GPU lock, no
  windowing. Sensors are point-in-time gauges (temperature, power, clocks,
  VRAM), so reading "now" is exactly the right semantics.
- AMD exposes **multiple temperature sensors per GPU** (`edge`, `junction`,
  `memory`), each a separate series with a `sensor` label sharing the same `id`.
  Dashboards must aggregate with `max`, not `sum`, or they double/triple-count.
- The metric **names overlap with the NVIDIA sampler** (`gpu_utilization`,
  `gpu_memory`, `gpu_clock`, `gpu_temperature`, `gpu_power_usage`, ...) by
  design — a vendor-neutral schema. The `vendor` label (`"amd"` / `"nvidia"`)
  distinguishes them.

### The PMU sampler (`gpu_amd_pmu`)

The PMU sampler is the focus of the rest of this document. Its design is shaped
by three hard facts about AMD device-level counters (each explained in detail
below):

1. **The hardware per-WGP counter registers are 32-bit and *saturate*** (clamp,
   not wrap) within ~34–275 ms of busy time — far shorter than a normal sample
   interval.
2. **The only way to reset/window a counter is `start_context` → `stop_context`**
   (the AQL start packet resets the registers); there is no read-and-reset and
   no in-API window parameter.
3. **By default each `start_context`/`stop_context` takes a ~18 ms KFD device-lock
   ioctl**, but setting `ROCPROFILER_DEVICE_LOCK_AT_START=1` acquires the lock
   once at config time, dropping per-cycle start/stop to **~150 µs**.

Given those, the sampler uses a **per-GPU background worker thread that brackets
each short window** (`rocprofiler.rs`):

```
# once at init: load libs, force-configure, hsa_init, build per-agent
# device-counting context + single-pass counter config.
# Set ROCPROFILER_DEVICE_LOCK_AT_START=1 BEFORE any rocprofiler/HSA init.

# per GPU, a worker thread loops forever:
start_context(ctx)          # resets the per-WGP counters to 0 (~38 us)
sleep(WINDOW = 40 ms)       # counters accumulate; lock NOT held during sleep
sample_device_counting()    # read this window's per-WGP deltas (~0.4 ms)
stop_context(ctx)           # freeze
accumulate the per-window delta into a shared running total

# refresh() just reads the shared running total — a cheap in-memory read,
# no GPU I/O. The totals are monotonic, so the viewer turns them into rates
# with rate() exactly like any other Rezolus counter.
```

Why this shape:

- **Bracketing each window resets the 32-bit registers**, so each per-window
  delta stays far below the saturation ceiling. Summing those deltas into a
  64-bit accumulator gives a monotonic counter that takes **~7 months** of full
  load to overflow (and `rate()` handles even that).
- **The worker sleeps (does not busy-wait) during the window**, so it costs
  ~1 % CPU. The ~150 µs of start/stop/read work per 40 ms iteration is
  negligible because of the device-lock-at-start flag.
- **`refresh()` does no GPU work** — it reads the in-memory accumulator under a
  mutex. The expensive GPU interaction is fully decoupled from the agent's
  sampling cadence.
- **rocprofiler is a single per-process tool**, so all worker threads serialize
  their start/sample/stop calls through one global `STATE` mutex; only the
  per-window sleep runs lock-free.

This replaced an earlier "start the context once and read the cumulative total
on demand" model. That model was simpler but **fundamentally broken**: a
continuously-running context never resets the 32-bit registers, so they pin at
their ceiling within the first second of uptime and read a flat value forever.
The windowed-worker model is what AMD's own `device_counting_sync_client.cpp`
sample does, for the same reason.

### Cost, privilege, and reliability notes that drive the design

- **Exclusive device lock.** Only one profiler can hold a GPU's profiling lock.
  If a second profiler (another `amd-pmc-watch`, `rocprofv3`, or a vLLM with
  profiling active) is running, it gets locked out and reads zeros. So the
  agent must be the only profiler on its GPUs.
- **`CAP_PERFMON`**, not full root (see "Privileges").
- **Stable power state.** On RDNA many SQ counters read 0 (or free-run with
  garbage) unless the GPU is pinned to a stable power level
  (`amd-smi set -g N -l stable_std`). The sampler does **not** change the power
  state by default — pinning clamps the clock (~1.6–1.85 GHz) and perturbs real
  workloads — so those counters only read correctly when an operator opts into
  the stable state. Clock/GRBM/SQ_WAVES/SQ_BUSY/GL2C counters work regardless.
- **An HSA runtime thread.** `hsa_init()` (required to arm rocprofiler, see
  below) spawns an HSA background thread that busy-polls a signal at ~100 % of
  one core. This is inherent to bringing up HSA at all — it appears identically
  in the bare `amd-pmc-watch` tool — and is the dominant CPU cost of enabling
  the PMU sampler, independent of our read model.

---

# Reading AMD GPU device counters without the HIP runtime

This note explains how Rezolus reads **device-wide AMD GPU hardware
performance counters** (the PMCs that `rocprofv3` programs) from the agent
process **without initializing the HIP runtime** and without instrumenting the
GPU workload. It is the design basis for the `gpu_amd_pmu` sampler
(`src/agent/samplers/gpu/linux/amd/pmu/`).

The telemetry layer (power, temperature, clocks, VRAM, SMI-level utilization)
is a separate concern handled by `rocm_smi.rs` / the `gpu_amd_smi` sampler and
summarized above; this note is only about the hardware counter layer.

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

### Core lifecycle

The context and counter config are built **once per agent**, then a per-GPU
worker thread **brackets each window** with start/stop (because the counters
saturate and must be read as per-window deltas — see "How the AMD device PMU
counters work"):

```
# setup (per agent), once
create_context
create_buffer  +  create/assign callback thread
configure_device_counting_service(... config-selection callback ...)
create_counter_config(agent, counter_ids, n, &config)   // resolve names -> ids
# (ROCPROFILER_DEVICE_LOCK_AT_START=1 set before init makes start/stop cheap)

# per-GPU worker thread, looping:
start_context                       // AQL start: reset counters to 0, begin counting
sleep(WINDOW = 40 ms)               // lock NOT held during the sleep
sample_device_counting_service(...) // read this window's per-instance values
stop_context                        // freeze
sum the per-instance records -> per-counter delta; add into a running total

# each refresh() (on demand): read the in-memory running total — no GPU I/O

# shutdown
join workers  +  hsa_shut_down
```

The implementation lives in
`src/agent/samplers/gpu/linux/amd/pmu/rocprofiler.rs`. The next two sections
walk through how counter events are configured and how reads work.

## How the AMD device PMU counters work

Understanding the read model requires understanding the hardware. The following
applies to RDNA (gfx10/11/12); CDNA differs in block names and some widths.

### Counters live in hardware blocks, replicated per instance

GPU performance counters are physical registers inside named hardware **blocks**:

| Block | What it covers | Counters Rezolus uses |
|---|---|---|
| **GRBM** | Graphics register bus manager — global front-end clocks | `GRBM_COUNT`, `GRBM_GUI_ACTIVE` |
| **SQ** | Sequencer — wavefronts, instruction issue (VALU/SALU/LDS), busy/wave cycles | `SQ_WAVES`, `SQ_BUSY_CYCLES`, `SQ_WAVE_CYCLES`, `SQ_INSTS_VALU/SALU/LDS` |
| **SQC** | Sequencer cache — instruction (and scalar) cache | `SQC_ICACHE_REQ`, `SQC_ICACHE_HITS` |
| **GL2C** | Graphics L2 cache slices — L2 hits/misses and L2↔VRAM traffic | `GL2C_HIT`, `GL2C_MISS`, `GL2C_EA_RDREQ`, `GL2C_EA_WRREQ` |
| **TA / TCP** | Texture addresser / L0 cache (not currently used) | — |

Each block is **physically replicated** across the GPU. An SQ counter is not one
register — it exists per **WGP** (Workgroup Processor, the RDNA compute-unit
pair), per **Shader Array (SA)**, per **Shader Engine (SE)**. On the gfx1201 test
part that is 4 SE × 2 SA × 4 WGP = **32 SQ instances**. GL2C is replicated per L2
slice (32 instances on the same part). So a single counter read returns **one
record per hardware instance**, each tagged with its `(SE, SA, WGP)` dimension
coordinates.

**Rezolus sums the per-instance records into one device-wide value per counter.**
(The dimension coordinates are available via
`rocprofiler_query_record_dimension_position` if per-WGP breakdown is ever
wanted, but the always-on agent reports the device sum.)

### The registers are narrow and saturate

Each per-instance register is **32-bit** and **clamps at `2^32 − 1`** on
overflow — it does **not** wrap. There is no overflow flag and no way to recover
the true count once clamped. This is the single most important fact for the
design:

- A summed device value therefore tops out at `N × (2^32 − 1)` — e.g. 32 ×
  (2³²−1) = 137,438,953,440 for the SQ counters. Seeing exactly that value means
  every instance has saturated.
- How fast a counter saturates depends on its accumulation rate, which depends
  on the workload. Measured on gfx1201 under load:
  - `SQ_WAVE_CYCLES` (sums wave-cycles across 32 WGPs at GPU clock): **~34–275 ms**.
  - `SQ_BUSY_CYCLES` (plain busy-cycle counter): **~1.8–2.7 s** (≈ `2^32 / clock`).
  - Plain instruction counters: somewhere in between, workload-dependent.

Because even the slowest of these saturates in seconds — well under a useful
cumulative sampling horizon — **counters cannot be read as a running total.**
They must be read as **per-window deltas**, where each window is short enough that
no instance overflows.

### Resetting a counter requires a start/stop cycle

The only way to zero the registers is the AQL **start packet**, which the SDK
emits inside `start_context` (the packet literally does *reset counters, then
begin counting*). The **read packet** (`sample_device_counting_service`) reads
the current values **without** resetting, and the **stop packet** freezes them.
There is no read-and-reset call and no window/interval parameter on the sample
API. So a correct windowed read is:

```
start_context   # AQL start packet: reset to 0, begin counting
... wait ...     # the window
sample           # AQL read packet: snapshot current per-instance values
stop_context     # AQL stop packet: freeze
```

This is exactly the loop the worker thread runs. The value delivered for each
record is a `double` (`rocprofiler_counter_record_t.counter_value`); the integer
hardware counts fit exactly below 2⁵³, and derived/ratio counters carry genuine
fractional values.

### Single-pass slot budget

A block has a small fixed number of physical counter slots, and the SDK programs
**one pass** (it does not time-multiplex). On gfx1201 the **SQ and SQC blocks
share an 8-slot register pool**, so `SQ(n) + SQC(m)` must be ≤ 8 or
`rocprofiler_create_counter_config` aborts with "Invalid Register used". GL2C
has its own budget (~4). This is why Rezolus's SQ/SQC set is exactly at the
ceiling (6 SQ + 2 SQC = 8) and adding any SQ/SQC counter requires dropping
another. Other blocks (GRBM, TA, TCP) have separate budgets with headroom.

### Stable power state

On RDNA, many SQ counters only read non-zero when the GPU is pinned to a stable
power level; at the default `auto` level they read 0 (or free-run with garbage).
The globally-accumulated counters (GRBM, `SQ_WAVES`, `SQ_BUSY_CYCLES`) and the
GL2C/SQC counters work regardless. See finding #3 below for details.

## Configuring the counter events

Configuring counters happens in two phases because rocprofiler-sdk imposes
ordering rules (context/config creation must happen inside the tool
`initialize` callback; `start_context` must happen after `hsa_init()`).

### Phase 1 — build the context, service, and counter config (`setup_agent`)

```
create_context(&ctx)                                     # rocprofiler_create_context
create_buffer(ctx, ..., LOSSLESS, noop_cb, &buf)         # rocprofiler_create_buffer
create_callback_thread(&th) ; assign_callback_thread(buf, th)
configure_device_counting_service(ctx, buf, agent,       # rocprofiler_configure_device_counting_service
                                  device_counting_cb)    #   registers the config-selection callback
```

At this point the device-counting service is wired to the context, but **no
specific counters are chosen yet**. Selecting them:

```
# 1. List every counter the agent supports, as opaque numeric IDs.
iterate_agent_supported_counters(agent, cb)              # rocprofiler_iterate_agent_supported_counters
# 2. Resolve each ID to its name to build a name -> id map.
for each id: query_counter_info(id, V0, &info)           # rocprofiler_query_counter_info  (reads info.name)
# 3. Pick the IDs for the counters WE want (the COUNTERS list); skip any the
#    agent does not support; also record id -> name for labeling records later.
# 4. Bundle the chosen IDs into a single counter config (a.k.a. profile). This
#    is where the per-block single-pass slot budget is enforced — too many
#    counters in one block makes this call fail.
create_counter_config(agent, ids.ptr, ids.len, &config)  # rocprofiler_create_counter_config
```

rocprofiler identifies counters by **numeric IDs, not names**, so steps 1–2
exist purely to translate our human-readable `COUNTERS` list (e.g.
`"SQ_INSTS_VALU"`) into the `rocprofiler_counter_id_t`s that
`create_counter_config` needs. The resulting `config`
(`rocprofiler_counter_config_id_t`) is stored per agent.

### Phase 2 — arm the config (`start_context`, after `hsa_init()`)

The config exists but isn't active until the context starts — and rocprofiler
**asks for the config via a callback** rather than taking it as an argument:

```
PENDING_CONFIG.set(agent.config.handle)   # thread-local: which config to use
start_context(ctx)                        # rocprofiler_start_context
   └─ rocprofiler SYNCHRONOUSLY invokes device_counting_cb on THIS thread:
          device_counting_cb(ctx, _agent, set_config, _) {
              handle = PENDING_CONFIG.get()
              set_config(ctx, config{handle})   # ← hands the config to rocprofiler
          }
PENDING_CONFIG.set(0)                     # clear it
```

This two-callback indirection is the non-obvious part: you don't pass counters
to `start_context` directly. You register a service callback in Phase 1
(`configure_device_counting_service`), and rocprofiler calls back into it during
`start_context` to *fetch* the config you want active. The config is delivered
to the callback through a **thread-local** (`PENDING_CONFIG`) because the
callback fires synchronously on the same thread that called `start_context`,
which avoids a global lock.

Once `start_context` returns, the chosen counters are programmed into the
hardware counter registers (in every instance of each block) and accumulating.

#### rocprofiler-sdk functions used for configuration

| Step | Function | Purpose |
| --- | --- | --- |
| 1 | `rocprofiler_create_context` | Create the collection container |
| 2 | `rocprofiler_create_buffer` | Output buffer for records |
| 3 | `rocprofiler_create_callback_thread` / `..._assign_callback_thread` | Buffer callback plumbing |
| 4 | `rocprofiler_configure_device_counting_service` | Attach the device-wide service + register the config-selection callback |
| 5 | `rocprofiler_iterate_agent_supported_counters` | List the agent's counters (as IDs) |
| 6 | `rocprofiler_query_counter_info` | Get each counter's name → build name↔ID map |
| 7 | `rocprofiler_create_counter_config` | Bundle chosen IDs into a config (enforces the slot budget) |
| 8 | `rocprofiler_start_context` | Activate; fires the callback that delivers the config via `set_config` |

## Reading the counters

The context started in Phase 2 runs continuously, so the hardware counters
accumulate from that point. Each read returns the **running cumulative total**
(values reset only on `stop_context`, which we do only at shutdown). This is why
there is no background thread and no per-read start/stop window: `refresh()`
just reads on demand.

```
sample(idx):                                          # called from refresh(), per GPU
    lock STATE                                        # serialize: rocprofiler rejects
                                                       #   overlapping reads (CONTEXT_ERROR)
    out = buffer[MAX_RECORDS]
    sample_device_counting_service(ctx, {}, NONE,     # rocprofiler_sample_device_counting_service
                                   out.ptr, &count)   #   writes one record PER HARDWARE INSTANCE
    # e.g. 4 SQ counters x 32 WGP instances = 128 records
    sums = {}
    for rec in out[..count]:
        query_record_counter_id(rec.id, &cid)         # rocprofiler_query_record_counter_id
        name = id_to_name[cid]                        #   which counter is this record?
        sums[name] += rec.counter_value               # ← SUM across instances
    return sums                                       # one value per counter (device-level)
```

Key points:

- **One read, many records.** `sample_device_counting_service` returns one
  `rocprofiler_counter_record_t` **per (counter × hardware instance)** — e.g.
  `SQ_INSTS_VALU` comes back as 32 records (one per WGP instance), each with a
  `counter_value` and dimension info.
- **Summed to device level.** The sampler sums all instances of a counter into a
  single number (`sums[name] += rec.counter_value`). The exposed metric (e.g.
  `gpmu_valu_instructions`) is that single sum, not 32 separate values. The
  per-instance breakdown (shader engine / shader array / WGP / L2-slice) is
  carried in the records' dimensions but currently discarded.
- **Reads are serialized.** Reads run inside `refresh()`, which executes
  concurrently with other samplers (and possibly overlapping scrapes).
  rocprofiler rejects overlapping `sample` calls on a context with
  `CONTEXT_ERROR`, so the read is taken under the `STATE` mutex — which also
  guards the shared library/agent state.
- **Cumulative → counter metric.** Because each value is a running total, the
  sampler publishes it into a monotonic `CounterGroup` (advancing it by the
  delta from its current value, since `CounterGroup` exposes `add`/`value` but
  no `set`). The viewer then derives rates.

#### rocprofiler-sdk functions used for reading

| Function | Purpose |
| --- | --- |
| `rocprofiler_sample_device_counting_service` | Read all per-instance counter records for the agent |
| `rocprofiler_query_record_counter_id` | Map a record back to the counter it belongs to |
| (`rocprofiler_stop_context` at shutdown) | Stop the context started at init |

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
3. Register the rocprofiler tool, build a single-pass counter config (names
   resolved per agent), `start_context` **once**, then call
   `sample_device_counting_service` on demand for the running cumulative totals
   (`stop_context` only at shutdown). See "Configuring the counter events" and
   "Reading the counters" above.
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

## 9. `sample_device_counting_service` leaks ~80 bytes per call (ROCm 7.2.1)

**There is a small memory leak inside rocprofiler-sdk's per-sample read path.**
Each `rocprofiler_sample_device_counting_service` call leaks roughly **80 bytes**
on ROCm 7.2.1 (gfx1201). This is a **library bug, not a Rezolus bug** — the
agent's own Rust code (`rocprofiler.rs`, `mod.rs`) allocates nothing per sample;
the per-window `out` buffer and the bounded 14-key accumulator are reused/freed.

How it was isolated:

- A standalone loop of **all the ROCm SMI getters** (`librocm_smi64.so`) over
  50,000 iterations does **not** leak — RSS is flat. So the SMI sampler
  (`gpu_amd_smi`) is clean.
- A loop of `start_context → sample → stop_context` (via `amd-pmc-watch`) leaks
  ~4 KB/s at a 20 ms interval (~80 B/sample).
- A **sample-only** loop on a *continuously running* context (no per-cycle
  start/stop) leaks at the **same** rate. This pins the leak on
  `sample_device_counting_service` itself, not on start/stop.
- Restarting the process reclaims everything (RSS returns to the ~130 MB
  baseline), confirming it is heap growth, not a fixed cost.

Likely source (from the SDK source, `counters/device_counting.cpp`
→ `agent_async_handler`): the per-read `EvaluateAST::evaluate(...)` /
`set_out_id(...)` path appears to accumulate state on the long-lived AST objects
each read. Not conclusively pinpointed; reported as a library issue.

**Impact at Rezolus's settings.** With one sample per 40 ms `WINDOW`
(≈25 samples/s/GPU), the leak is **~0.18 GB/day** (~7 MB/hour). An agent left
running ~11 days reaches ~2 GB — matching the observed growth.

**Why it cannot be fixed in-process.** rocprofiler contexts can only be created
inside the `tool_initialize` callback (everything else returns
`CONFIGURATION_LOCKED`), and there is **no `rocprofiler_destroy_context`**. So the
agent cannot tear down and rebuild the counting context mid-run to reclaim the
leaked memory — the only full reclaim is a process restart.

**Mitigation options (none applied yet — this is a documented finding):**

- *Duty-cycle the sampler* — measure one short window then idle for the rest of
  the interval, sampling ~1×/s instead of ~25×/s (a ~25× leak reduction to
  ~7 MB/day). Trade-off: the published counters would reflect only the short
  counting window, so the rate-based dashboards undercount unless the delta is
  scaled by `interval/window`, which adds extrapolation error.
- *Bigger `WINDOW`* (e.g. 150 ms) keeps rates continuous and correct and cuts the
  leak ~4× (~45 MB/day), but risks 32-bit saturation under very heavy load.
- *Operational cap* — since `gpu_amd_pmu` is opt-in, run the systemd unit with a
  `MemoryMax=` + `Restart=on-failure` so the agent is recycled before it grows
  unbounded. The SMI sampler is unaffected and can stay always-on.

The right long-term fix is upstream in rocprofiler-sdk.

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
