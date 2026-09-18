# Task 1 report: Linux hardware sensors sampler

## Result

Task 1 adds an opt-in, read-only Linux `sensors` sampler backed directly by
sysfs. It discovers thermal zones, hwmon devices, and cooling devices, and
publishes the seven specified metric families:

| Metric | Unit |
| --- | --- |
| `sensor_temperature` | millidegrees Celsius |
| `sensor_power` | microwatts |
| `sensor_voltage` | millivolts |
| `sensor_current` | milliamps |
| `sensor_fan_speed` | RPM |
| `sensor_fan_pwm` | 0–255 |
| `sensor_cooling_state` | driver-defined state index |

Each family has its own gauge group and acquisition group. Reads are
family-major, and a successful family pass stamps its window after values have
been published. A pass where every active read fails clears live gauges and
discards the candidate window. A removal-only pass stamps the membership
change. Slots are stable and never reused; an identical sensor can reclaim its
existing slot after rediscovery, while a new identity consumes a new slot. The
lifetime bound is 256 identities per family.

Initialization performs no sysfs reads. `refresh()` applies an independently
configurable read throttle (5 seconds by default), dispatches one blocking
worker, and never overlaps workers. The worker rediscoveries capabilities every
60 seconds. A drop guard releases the in-flight latch even if the worker
panics. Discovery and read timing are logged separately from refresh dispatch
timing. The module-level documentation records why the direct sysfs access is
an allowed sampler-principle exception and why the configurable background
cadence is needed.

The sampler is opt-in even when `[defaults] enabled = true`. Non-Linux builds
retain the metric schema without registering a platform sampler. Analysis
attribution maps all `sensor_*` metrics to the `sensors` subsystem, including
the domain alias and golden-record coverage list.

## Discovery and decoding

Discovery accepts an arbitrary sysfs root for fixture and synthetic filesystem
tests. Published source and device identities are based on canonical Linux
device paths and do not contain the supplied test root. Cached input, enable,
and fault paths are also pinned to canonical device directories so reuse of a
`hwmonN` class symlink cannot redirect an old descriptor to a replacement
device.

The generic decoder handles standard hwmon temperature, direct power, bus
voltage/current, fan input, fan PWM, thermal-zone temperature, and cooling
state. Genuine hwmon channel enable and fault attributes gate values. A
`pwmN_enable` file is a control-mode selector and does not gate the measured
PWM command. A thermal-zone `mode=disabled` prevents kernel trip handling but
does not make the temperature unreadable, so it also does not gate the
measurement. PWM values outside 0–255 and malformed values are absent.

Driver-specific handling includes:

- INA3221 uses only bus-voltage and current channels 1–3. It excludes shunt
  voltages, summation channels, and ambiguous `curr4`; derived power uses
  checked `mV * mA = µW` multiplication and is marked as derived.
- INA238 excludes `in0_input` shunt voltage and exposes `in1_input` as bus
  voltage.
- NVIDIA `pwm_tach` accepts only the `rpm` input as fan speed.
- Labels recognized as NC or unconnected are skipped.
- Direct power is preferred when a device exposes it.

The exact verified Thor model and compatible tuple enables one documented
INA238 system-power scope annotation. Generic channels work without any Tegra
identity, and synthetic Orin data receives no Thor scope. Board model and SoC
compatibility are preserved in descriptor metadata and metric labels when
available, and they participate in identity so a slot is never relabeled.

## Test-driven development evidence

The initial decoder test command was:

```text
CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo test --locked -p rezolus sensors::linux::tests -- --test-threads=1
```

It compiled and ran five fixture/decoder tests; all five failed against the
empty discovery implementation. After lifecycle stubs were added, the same
command ran ten tests with five passing and five failing: stable-slot,
publication, throttle, and panic-latch behavior was still unimplemented.

Additional focused red tests caught concrete semantic and integration gaps:

- PWM and thermal-mode tests failed while generic `*_enable != 0` gating was
  incorrectly applied to control-mode attributes.
- Board-context tests failed until model and compatible metadata were carried
  into descriptors and publication.
- The class-index reuse regression read `99000` through a cached descriptor
  whose original device read `41000`.
- The required-metadata regression returned zero discovery errors instead of
  the expected two errors for malformed/missing thermal and cooling `type`
  files.
- The opt-in config test showed `sensors` enabled through defaults.
- Sampler attribution tests reported both a missing expected subsystem and an
  unattributed `sensor_power` metric.
- A PWM range regression accepted an out-of-range value before validation was
  added.

The final focused verification on the completed source was:

```text
CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo test --locked -p rezolus sensors -- --test-threads=1
```

Result: **13 passed, 0 failed**. These tests cover the supplied Thor inventory,
standard and driver-specific decoding, malformed/disabled/faulted inputs,
synthetic Orin behavior, canonical path pinning, required metadata errors,
stable bounded slots, removal and recovery, family windows, throttles,
panic-safe dispatch, and an actual asynchronous refresh/non-overlap path over a
temporary sysfs tree.

Fresh integration checks also passed:

```text
CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo test --locked -p rezolus agent::samplers::attribution_tests -- --test-threads=1
# 8 passed, 0 failed

CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo test --locked -p rezolus agent::config::tests -- --test-threads=1
# 6 passed, 0 failed

CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo test --locked -p rezolus analysis::extract::golden::tests::golden_record_matches_captured_expectation -- --test-threads=1
# 1 passed, 0 failed

CARGO_TARGET_DIR=/home/yao/workspace/rezolus/target cargo build --locked --bin rezolus
# finished successfully

cargo fmt --all -- --check
git diff --check
# both clean
```

The checked-in `thor-sensors.json` was normalized with `jq -S` and compared to
`/tmp/thor-sensors-fixture.json`; the normalized files were identical.

## Independent discovery review

The independent review in `task1-discovery-review.md` raised two findings, and
both are addressed:

1. Cached paths could follow a reused class symlink and read a replacement
   device under the old identity. All read/control/fault paths are now built
   from the canonical device node, with a symlink-retarget regression.
2. Thermal and cooling `type` errors were silently dropped. Both discovery
   loops now return path-qualified errors, with malformed and missing metadata
   coverage.

The review re-examined both fixes and reported no new breakage.

## Local live evidence

An 80-second sensors-only x86 Linux run exercised the real `/sys` path through
two discovery passes and 16 read sweeps (80 scrapes). It found 58 identities;
56 were present. All 29 discovered temperatures were present, 27 of 28 cooling
states were present, and the one fan input failed with `ENODEV` and therefore
had no acquisition window. The partial cooling failure was absent rather than
zero. Read warnings were emitted once on the state transition rather than on
every sweep.

Measured refresh dispatch was 3–67 µs (median 12 µs), blocking reads were
20,200–43,124 µs (median 40,766 µs), and discovery was 3,103–5,982 µs. This run
used the same scheduling/discovery implementation as the final source; the only
subsequent production change was PWM bounds validation. These local x86 values
do not establish Thor hardware performance.

## Diagnostics and remaining limits

State-change logs cover no sensors found, discovery/permission errors, slot
overflow, partial/all family read failures, recovery, overdue work, and worker
panic. Rezolus currently has no generic runtime degraded-status setter, so
sensor health is logs-only. Missing and failed metrics are still cleared from
the live gauge groups.

A wholly failed family deliberately retains its last successful acquisition
window while clearing current gauges. Recorders that deduplicate solely by an
unchanged window may retain a previous recorded value; end-to-end recording
outage behavior needs separate validation.

Derived INA3221 power is based on sequential voltage and current reads and is
not an atomic electrical sample. Raw temperature sources may overlap between
thermal and hwmon, but their native source identities remain distinct; this
task does not reconcile duplicates across drivers or samplers. Cooling state is
a driver state index and does not prove the absence or presence of hardware
throttling.

The design intentionally omits configurable limits, trip points, thermal
modes, fan-control modes, overcurrent events, and hardware writes. A system
call may stall the single blocking worker, but the latch prevents worker
accumulation. Capability changes can take up to 60 seconds to appear. Reaching
the 256-identity lifetime cap leaves later identities unpublished and produces
a transition diagnostic.

Thor live timing, reboot/driver reload behavior, Orin hardware, alternate
JetPack versions, and fleet-scale operation remain unvalidated. The supplied
Thor inventory validates filesystem layout and decoding only.
