# `gpu_amd_smi` sampler design

The `gpu_amd_smi` sampler collects per-GPU telemetry from AMD GPUs — memory,
utilization, temperature, power, energy, clocks, and PCIe throughput — via the
ROCm System Management Interface library (`librocm_smi64.so`). It lives at
`src/agent/samplers/gpu/linux/amd/` and is the AMD counterpart to the NVIDIA
`gpu_nvidia` sampler (`nvml-wrapper` / NVML).

This note covers the **SMI telemetry layer**. Device-wide hardware performance
counters (the PMCs `rocprofv3` programs) are a separate, planned layer
described in [`amd_gpu_counters.md`](amd_gpu_counters.md).

## Goals and constraints

1. **Same metric surface as NVIDIA.** Emit the existing `gpu_*` metric names so
   dashboards and PromQL work across vendors, distinguished only by a
   `vendor="amd"` / `vendor="nvidia"` label.
2. **No build-time ROCm dependency.** Rezolus must keep compiling on stock CI
   runners (Ubuntu, macOS) that have no ROCm installed. A `-sys` crate that runs
   `bindgen` against the ROCm headers, or that links `librocm_smi64.so` at build
   time, would break those builds.
3. **Graceful on non-AMD hosts.** With the sampler enabled on a host that has no
   AMD GPU or no ROCm runtime, the agent must keep running and simply not emit
   AMD metrics — never fail or crash.

## Key decision: dlopen ROCm SMI at runtime

The NVIDIA sampler's `nvml-wrapper` crate `dlopen`s `libnvidia-ml.so` at
**runtime**, which is exactly why Rezolus compiles on machines with no NVIDIA
driver. We mirror that model for AMD: rather than depend on a ROCm crate, we
`dlopen` `librocm_smi64.so` ourselves via the `libloading` crate (already a
transitive dependency through `nvml-wrapper`) and declare only the ~12 C
functions we use.

Alternatives that were considered and rejected:

| Option | Why not |
| --- | --- |
| `amdgpu-sysfs` (pure Rust, reads `/sys`) | On real AMD datacenter hardware (verified on a Radeon AI PRO R9700 / gfx1201) the hwmon `*_input` nodes and even the `gpu_metrics` blob return `EBUSY`; this path would yield only VRAM + PCIe link width. |
| `rocm_smi_lib` / `rocm_smi_lib_sys` crates | `build.rs` runs `bindgen` against the ROCm headers, requiring ROCm to be installed wherever Rezolus is *compiled*, including CI. Violates constraint 2. |
| **dlopen `librocm_smi64.so`** (chosen) | Zero build-time ROCm dependency; full telemetry at runtime on AMD hosts; clean degradation when the library is absent. |

The cost of this choice is that we hand-mirror the C ABI (function signatures
and struct layouts). Getting that wrong does not fail to compile — it corrupts
memory at runtime. See "The FFI hazard" below.

## Structure

Three files, mirroring the NVIDIA sampler's shape:

- **`rocm_smi.rs`** — the FFI shim. A `RocmSmi` struct owns the dlopen handle
  (`Box<Library>`, declared *last* so it drops last) plus typed `libloading`
  `Symbol` function pointers. Construction loads the library (trying
  `librocm_smi64.so` then `librocm_smi64.so.1`), calls `rsmi_init(0)`, and
  resolves symbols: required getters error if missing, optional ones
  (`rsmi_dev_current_socket_power_get`, energy, PCIe) degrade to `None`. It
  exposes safe typed methods (`temperature`, `power_milliwatts`, `clock_hz`, …)
  that wrap the `unsafe` calls and convert units (millidegrees → °C, microwatts
  → mW, Hz, bytes). `Drop` calls `rsmi_shut_down()`.
- **`stats.rs`** — metric definitions via metriken's `#[metric]` macro. Reuses
  the NVIDIA `gpu_*` names, each with `vendor = "amd"`. Temperatures carry a
  `sensor` label (`edge`/`junction`/`memory`); clocks a `clock` label.
- **`mod.rs`** — the sampler. `const NAME = "gpu_amd_smi"`, registered via the
  `#[distributed_slice(SAMPLERS)]` `SamplerEntry`. `init()` returns `Ok(None)`
  (→ status *disabled*, not *failed*) when ROCm/the GPU is absent.
  `AmdInner::refresh()` locks a `Mutex`, loops over devices, and writes
  `.set(id, value)` on each successful getter, ignoring per-call errors — the
  same pattern as the NVIDIA `refresh_nvml()`.

## Metrics

All are `GaugeGroup`/`CounterGroup` indexed by device id (0..N), labelled
`vendor="amd"`. The ROCm SMI device order equals the index used by the `amd-smi`
/ `rocm-smi` CLIs.

| Metric | Labels | Source |
| --- | --- | --- |
| `gpu_memory` | `state=free\|used` | `rsmi_dev_memory_total/usage_get` (free = total − used) |
| `gpu_utilization` | — | `rsmi_dev_busy_percent_get` |
| `gpu_memory_utilization` | — | `rsmi_dev_memory_busy_percent_get` |
| `gpu_temperature` | `sensor=edge\|junction\|memory` | `rsmi_dev_temp_metric_get` |
| `gpu_power_usage` | — | `rsmi_dev_current_socket_power_get`, falling back to `rsmi_dev_power_ave_get` |
| `gpu_energy_consumption` (counter) | — | `rsmi_dev_energy_count_get` |
| `gpu_clock` | `clock=graphics\|compute\|memory` | `rsmi_dev_gpu_clk_freq_get` |
| `gpu_pcie_throughput` | `direction=receive\|transmit` | `rsmi_dev_pci_throughput_get` |

NVIDIA-only metrics (`gpu_sm_*`, `gpu_tensor_*`, `gpu_dram_bandwidth_utilization`)
have no ROCm SMI equivalent and are not emitted.

## The FFI hazard (and the bug it caused)

Because we hand-declare the C ABI, a wrong struct layout is silent at compile
time and catastrophic at runtime. This bit us once: the Rust `rsmi_frequencies_t`
mirror originally omitted the leading `bool has_deep_sleep` field present in the
C struct:

```c
typedef struct {
  bool     has_deep_sleep;   // was missing on the Rust side
  uint32_t num_supported;
  uint32_t current;
  uint64_t frequency[RSMI_MAX_NUM_FREQUENCIES];
} rsmi_frequencies_t;
```

The Rust struct was therefore *smaller* than the C type, so
`rsmi_dev_gpu_clk_freq_get` wrote past the stack buffer — **intermittent
segfaults under sustained sampling** — and the shifted offsets made
`current`/`frequency` read garbage, so the reported clock was the DPM ceiling
instead of the live value. Adding the field fixed both symptoms.

The lesson is encoded in the code: any `#[repr(C)]` mirror in `rocm_smi.rs`
carries a comment that it must match the C layout byte-for-byte, including
padding, and the values are cross-checked against `amd-smi` (see Validation).

## Validation

The sampler was validated against AMD's own tooling under real load on a host
with a Radeon AI PRO R9700 (discrete) and a Raphael APU:

- Run the agent (`gpu_amd_smi` only) and `rezolus record` its endpoint while a
  vLLM workload drives the R9700, capturing `amd-smi monitor --csv` in parallel
  as ground truth. (`amd-smi` is used rather than `rocm-smi` because the
  gfx1201 card reports `N/A` through the `rocm-smi` CLI but full data via
  `amd-smi`.)
- A comparison step aggregates each per-second series and checks agreement
  per device. Under 100%-utilization load the sampler matched `amd-smi` within
  tolerance on power, temperature, utilization, VRAM%, and clocks.

Graceful-degradation was confirmed on a host with an NVIDIA RTX 5080 and **no**
AMD GPU / ROCm: with `gpu_amd_smi` enabled it reports *disabled* (not *failed*),
`gpu_nvidia` stays *active*, and the agent runs normally.

## Operational notes

- Enabled by default via `config/agent.toml` (`[samplers.gpu_amd_smi]`); the
  default-enable is safe everywhere because absent ROCm/AMD → *disabled*.
- ROCm SMI getters are not assumed to be thread-safe; the sampler serializes its
  own calls behind a `Mutex<AmdInner>` and only `gpu_amd_smi` touches the
  library.
- `rsmi_dev_gpu_clk_freq_get` reports the current DPM clock *level*, which on
  some GPUs is the ceiling rather than the instantaneous frequency. This is a
  known limitation of the SMI clock path; the `amd-smi`/`gpu_metrics` table is
  the more precise source if exact instantaneous clocks are needed later.
