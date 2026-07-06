# Drive health sampler — Phase 1: all-drive temperature (module-free)

- **Opened:** 2026-07-06
- **Status:** GO — Phase 1 shipped. Temperature via **pass-through ioctls, no
  kernel module** (re-scoped from hwmon mid-effort by owner). SATA path
  hardware-verified against `smartctl`; NVMe parser fixture-verified (no NVMe
  host). Phases 2–3 (SMART health, SAS) remain OPEN. See Outcome.
- **Arc:** "All-drive health" (NVMe + SATA/SAS), delivered in phases. This entry
  is Phase 1 (temperature).
- **Owner:** Brian Martin

This entry doubles as the design spec (we exercise the journal as the single
decision record rather than a separate spec doc).

## Motivation

Identified operational need: track **NVMe drive temperature**, broadened to
"drive health" across all drive types since the mechanism generalizes. Rezolus
is grounded in high-resolution *performance* telemetry; a slow-moving health
gauge is a deliberate widening of scope — see "Fit with principles".

## Design pivot: why not hwmon / `drivetemp` (grounded)

The first cut used hwmon (`/sys/class/hwmon`): the `nvme` driver exposes
temperature there natively, and the `drivetemp` module does the same for
SATA/SAS. That delivered NVMe module-free, but SATA/SAS temperature **only**
appears in hwmon when `drivetemp` is loaded — and `drivetemp` is not loaded by
default. Two findings killed that path for SATA:

1. **It needs a module.** Requiring operators to `modprobe drivetemp` fleetwide
   is a non-starter; a telemetry agent should not depend on a non-default sensor
   module being present. (Owner requirement: **the sampler must need no module.**)
2. **It is expensive.** Measured with the hwmon cut on a 22-drive SATA host
   (`drivehealth sampling latency` debug line): **~83 ms per refresh**, ~3.8 ms
   per drive — because each `drivetemp` read issues an ATA command to the
   physical drive. (Principle 16: the number, not an adjective.)

**Module-free alternative (chosen):** read temperature directly from the drive
via **pass-through ioctls** — the mechanism `smartctl`/`hddtemp` use, over the
block device that already exists, no sensor module:

- **SATA (incl. SATA behind a SAS HBA):** `SG_IO` **ATA PASS-THROUGH(16)**
  wrapping `SMART READ DATA` (ATA `0xB0`, feature `0xD0`); parse the attribute
  table for id 194 (`Temperature_Celsius`), fallback 190.
- **NVMe:** `NVME_IOCTL_ADMIN_CMD` → **Get Log Page 0x02** (SMART/Health);
  Composite Temperature (Kelvin → °C). This is the same log page Phase 2 needs.
- **SAS (true SCSI):** `SG_IO` **LOG SENSE** page `0x0D`. *Deferred* — no
  SAS-only hardware to verify against here.

**Grounding:** on the GO-check host with `drivetemp` NOT loaded, ATA
pass-through SMART reads `/dev/sda` = 38 °C (attr 194 and SCT agree), `/dev/sdb`
= 34 °C, `/dev/sdk` = 41 °C — cross-checked with `smartctl -A`. Confirms
module-free temperature on real hardware.

## Arc and phasing

- **Phase 1 (this entry): all-drive temperature via pass-through ioctls.**
  SATA (ATA pass-through) + NVMe (admin Get Log Page 0x02). Module-free.
- **Phase 2 (later): NVMe SMART-log health** — wear (`percentage_used`), spare,
  critical-warning bits, media errors, power-on hours — extends the same NVMe
  Get Log Page 0x02 read.
- **Phase 3 (later): ATA/SATA + SAS SMART attributes** — reallocated sectors etc.
  (ATA), and SAS LOG SENSE temperature/health.

Each phase is its own journal entry → PR.

## Safety posture (raw device commands)

The sampler issues **only read-only diagnostic commands** — ATA `SMART READ
DATA`, NVMe `Get Log Page`, (later) SCSI `LOG SENSE`. No writes, no state
changes, no destructive opcodes. Device nodes are opened read-only. This is
called out because pass-through ioctls can express dangerous commands; this
sampler must never construct one.

## Goal (Phase 1)

Expose per-drive temperature as a gauge for every NVMe and SATA drive, read via
pass-through ioctl with **no kernel module**, at a low, **measured** per-refresh
cost.

## Requirements

1. Linux-only userspace gauge sampler `drivehealth` under
   `src/agent/samplers/drivehealth/`, registered via `SAMPLERS`. macOS no-op.
2. Discover drives **once at startup** by walking `/sys/block` (skip
   loop/dm/md/zd/ram/sr); classify NVMe vs ATA; resolve the `/dev` node and
   `model`/`serial` from sysfs (one-time sysfs discovery is blessed by
   principle 15). No module is loaded.
3. Emit one metric:
   ```
   drive_temperature{device, type, model, serial}   # degrees Celsius, i64
   ```
   - `device` = kernel name (`nvme0`, `sda`); `type` = `nvme` | `sata`.
   - `model`, `serial` read once at discovery. **Serial is potentially
     sensitive** — included for stable cross-reboot fleet identity, documented
     in the metric help.
4. Temperature read via pass-through ioctl per bus (ATA / NVMe). The buffer
   **parsers** are pure and unit-tested against fixture bytes; the ioctl glue is
   thin unsafe code, hardware-verified.
5. **Cadence (Principle 17).** These are device commands, so reads are throttled
   to a configurable `interval` (default 60s) and dispatched **off** the sample
   cycle (`spawn_blocking`), all drives in parallel; `refresh()` serves the
   cached value. Keeps `refresh()`'s sample-cycle contribution ~µs.
6. Robust to absence / permission error: no drives or `EACCES` → zero series,
   never errors the agent. A per-refresh read failure skips that drive that tick.
7. **Overhead is measured** on real hardware and reported in close-out
   (Principle 16).

## Fit with principles

Drive temperature has **no** BPF or perf hook — it originates in the drive's own
health data — so reading it from the device is the legitimate principle-15
exception (documented in-module). One-time discovery via sysfs is blessed.
Because the read is an expensive device command (not a free mmap load), it is
throttled and decoupled from the scrape cadence — the documented principle-17
departure from principle 10. Prior art: the GPU samplers are non-BPF gauge
samplers reading temperature via vendor SMI; this is the same shape.

## GO / NO-GO criteria

**GO if all hold:**
- On the SATA host: `drive_temperature{type=sata,...}` matches `smartctl -A`
  (attr 194) per drive, with correct `device`/`model`/`serial` labels.
- NVMe parser validated against a spec-derived fixture (Composite Temp bytes);
  hardware validation on an NVMe host is a **reopen condition** (none available).
- **Measured** `refresh()` sample-cycle cost is ~µs (reads off-cycle); the
  measured full-sweep device-read cost is reported.
- No drive / `EACCES` → zero series, no error; macOS no-op.

**NO-GO / rethink if:**
- Pass-through SMART is unavailable on target hardware (rare; even USB bridges
  often pass ATA through). Reopen: fall back per-bus or document the gap.

## Plan

1. Land this redesign (done).
2. `device.rs`: `/sys/block` enumeration + classification + `/dev` node + labels.
3. `ata.rs`: ATA PASS-THROUGH(16) `SMART READ DATA`; pure parser (attr 194/190)
   TDD'd against fixture bytes; ioctl glue.
4. `nvme.rs`: admin Get Log Page 0x02; pure parser (Composite Temp) TDD'd.
5. Wire into the sampler shell (keep throttle + `spawn_blocking` + gauge; drop
   `hwmon.rs`).
6. Hardware-verify SATA against `smartctl`; **measure** overhead.
7. Close out with observed values + measured numbers.

## Open questions / limitations

- **SAS (true SCSI) LOG SENSE** deferred — no SAS-only hardware to verify.
- **NVMe** parser fixture-verified only (no NVMe host); hardware validation is a
  reopen condition.
- **Hotplug:** startup-only discovery; drives added later are not picked up.
  Acceptable for Phase 1; documented.
- **Privilege:** pass-through ioctls need `CAP_SYS_RAWIO` / root — the agent
  already runs privileged for eBPF. On a host where the agent is unprivileged,
  reads fail closed (zero series), never error.

## Outcome (2026-07-06) — GO, shipped

Implemented under `src/agent/samplers/drivehealth/linux/`: `device.rs`
(sysfs enumeration + per-drive read dispatch), `ata.rs` (SG_IO ATA
PASS-THROUGH → SMART attr 194), `nvme.rs` (admin Get Log Page 0x02), `mod.rs`
(sampler shell: throttle + `spawn_blocking` + gauge). `hwmon.rs` removed. Pure
parsers are unit-tested (ATA attribute table, NVMe Composite Temp,
enumeration); ioctl glue is hardware-verified. 10 unit tests + 3 ignored
hardware tests.

**Hardware verification (GO-check host, 23 SATA drives behind a SAS HBA,
`drivetemp` NOT loaded):**
- **Enumeration:** 23 drives discovered from `/sys/block`, no module loaded.
- **Reads:** 23/23 via ATA pass-through, values 19–43 °C, matching
  `smartctl -A` per drive (e.g. `sda` 38, `sdb` 34, `sdk` 41). No `drivetemp`.
- **Async path:** `refresh()` on a tokio runtime dispatched `spawn_blocking` and
  populated all 23 gauge values (integration test
  `hardware_refresh_populates_gauge`).
- **Fail-closed:** run unprivileged, the ioctls return `EACCES` and reads yield
  `None` → zero series, no error (observed). Pass-through needs
  `CAP_SYS_RAWIO`/root; the agent already runs privileged for eBPF.
- **NVMe:** parser fixture-verified (Kelvin→°C); no NVMe host available —
  hardware validation is a **reopen condition**.

**Overhead — measured (Principle 16):**
- `refresh()` contribution to the sample cycle: **~2 µs** (it only does a time
  check and dispatches; reads are off-cycle).
- Full-sweep device read: **~176 ms for 23 drives (~7.6 ms/drive)**, once per
  `interval`, on the blocking pool. The SAS HBA serializes pass-through
  commands, so the parallel reads do not fully overlap — acceptable off-cycle.
- This is *why* the read must be throttled + decoupled (Principle 17): at the
  10 ms TTL scrape cadence a 176 ms synchronous sweep would dominate the agent
  and hammer every drive with a command. Cadence is configurable
  (`[samplers.drivehealth] interval`, default 60s).

**Docs updated in the shipping change:** `docs/metrics.md`, `config/agent.toml`,
`docs/principles.md` (principles 16/17 examples), and this entry.

## Superseded design note

An earlier hwmon-based cut of this sampler was implemented and reached GO for the
SATA path *with `drivetemp` loaded* (22 drives, `drive_temperature` 31–41 °C,
correct labels), which is what surfaced the ~83 ms measurement and the
cadence/throttle design (both carried forward). It was replaced because it needed
`drivetemp` for SATA. The throttle + `spawn_blocking` + configurable `interval`
machinery and the parallel-read design are retained unchanged; only the
per-drive *read backend* changes from hwmon file reads to pass-through ioctls.
