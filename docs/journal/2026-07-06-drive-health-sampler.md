# Drive health sampler — Phase 1: all-drive temperature

- **Opened:** 2026-07-06
- **Status:** OPEN — intent landed before build (design approved; not yet implemented)
- **Arc:** "All-drive health" (NVMe + SATA/SAS), delivered in phases. This entry is Phase 1.
- **Owner:** Brian Martin

This entry doubles as the design spec for Phase 1 (we are exercising the journal
as the single decision record rather than keeping a separate spec doc).

## Motivation

Identified operational need: track **NVMe drive temperature**. Broadened during
scoping to "drive health" across all drive types, since the mechanism generalizes.
Rezolus is grounded in high-resolution *performance* telemetry, so a slow-moving
health gauge is a deliberate widening of scope — see "Fit with principles" below
for why it is legitimate rather than a workaround.

## Arc and phasing

"All-drive health" spans three independent data sources with increasing
complexity. We deliver walking-skeleton first:

- **Phase 1 (this entry): all-drive temperature via hwmon.** One sampler + device
  discovery + temperature for NVMe *and* SATA/SAS in a single cut. hwmon exposes
  temperature uniformly for both (the `nvme` driver and the `drivetemp` module),
  so temperature — the identified need — is reachable everywhere with plain file
  reads and no admin ioctls.
- **Phase 2 (later): NVMe SMART-log health.** Wear (`percentage_used`), available
  spare, critical-warning bits, media errors, power-on hours — via NVMe Get Log
  Page 0x02 (admin passthrough ioctl).
- **Phase 3 (later): ATA/SATA SMART attributes.** Vendor-specific attribute
  parsing (reallocated sectors, etc.).

Each phase is its own journal entry → PR.

## Goal (Phase 1)

Expose per-drive temperature as a gauge, for every NVMe and SATA/SAS drive the
kernel surfaces via hwmon, with low, bounded per-refresh cost.

## Requirements

1. New Linux-only userspace gauge sampler `drivehealth` under
   `src/agent/samplers/drivehealth/`, registered via the `SAMPLERS` `linkme`
   slice. macOS builds a no-op (pattern of existing Linux-only samplers).
2. Discover drives **once at startup** by walking `/sys/class/hwmon/*`; keep
   entries backed by a block drive (NVMe controller or a disk exposing
   `drivetemp`). Resolve the kernel device name (`nvme0`, `sda`).
3. Emit one metric:
   ```
   drive_temperature{device, type, model, serial}   # degrees Celsius, i64
   ```
   - `device` = kernel name (`nvme0`, `sda`).
   - `type` = `nvme` | `sata`.
   - `model`, `serial` = read once at discovery. **Serial is potentially
     sensitive** — included deliberately for stable cross-reboot fleet identity;
     documented in the metric help so operators know it is emitted.
   - Composite temperature only (per-sensor `sensor=` label deferred).
   - hwmon reports millidegrees C; divide by 1000. Unit convention matches the
     existing `gpu_temperature` gauge (Celsius, i64).
4. Backed by `GaugeGroup::new(MAX_DRIVES)` (cap 64), `.set(idx, celsius)` per
   refresh, per-index labels via `insert_metadata(idx, key, value)` — the exact
   mechanism the GPU sampler uses (`src/agent/samplers/gpu/linux/nvidia/`,
   `src/agent/metrics/mod.rs`).
5. Robust to absence: no hwmon / no drives / permission error → sampler starts
   with zero drives and emits no series, never errors the agent (GPU posture when
   no device present). A per-refresh read failure skips that drive that tick.
6. Unit tests for hwmon parsing against fixture sysfs trees.

## Fit with principles (why sysfs here is legitimate)

Principle 15 ("prefer BPF/perf over parsing procfs/sysfs") carves out an explicit
exception: *"A small set of metrics genuinely have no useful BPF or perf hook —
some kernel-internal gauges only surface through procfs. Those keep parsing the
file, but the choice should be deliberate, not the default."* Drive temperature
originates in the drive controller's health data; there is **no** BPF or perf
hook for it. Discovery is one-time (explicitly blessed). Per-refresh we read only
small `tempN_input` files, and temperature changes on the order of seconds — so
the high-sampling-frequency cost concern the principle targets does not apply.
The sampler module will carry a comment stating this deliberately.

Prior art that this mirrors: the GPU samplers are already non-BPF gauge samplers
that read temperature (`gpu_temperature`) via vendor SMI. This is the same shape,
one category over.

## GO / NO-GO criteria

**GO if all hold on a Linux host with at least one NVMe drive:**
- `drive_temperature` appears with a plausible value (roughly 20–80 °C) and
  correct `device`/`type`/`model`/`serial` labels.
- Per-refresh cost is bounded (no directory walk on the hot path; only the
  small `tempN_input` reads for discovered drives).
- Agent starts cleanly on a host with **no** supported drive (zero series, no
  error), and on macOS (no-op).

**NO-GO / rethink if:**
- hwmon does not expose the drives in target environments (e.g. `drivetemp` not
  loaded and no NVMe hwmon) — then temperature needs the NVMe SMART-log ioctl,
  which would fold Phase 1 into Phase 2. Reopen condition: if hwmon coverage is
  insufficient, promote the ioctl path earlier.

## Plan

1. Scaffold `drivehealth` sampler (mod.rs + linux/ + stats.rs), macOS no-op.
2. hwmon discovery + drive-name/model/serial resolution (with fixtures).
3. `drive_temperature` gauge group; wire refresh.
4. Unit tests for parsing; register and run on real NVMe hardware for GO check.
5. Close out this entry with the outcome (GO + observed values, or NO-GO +
   mechanism) in the implementing PR.

## Open questions / limitations (Phase 1)

- **Hotplug:** drives added after startup are not picked up (startup-only
  discovery). Acceptable for Phase 1; documented. Revisit if hotplug matters.
- **hwmon coverage** varies by kernel/driver (`drivetemp` must be loaded for
  SATA). Measured during GO check; drives the Phase-1-vs-Phase-2 boundary.
