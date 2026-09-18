# Linux hardware sensors, initially validated with a Thor inventory

Issue #1203 needs CPU/SoC temperatures, rail power, and fan telemetry beyond
NVML. The approved design has a shared sysfs reader, driver-specific decoding,
and optional board interpretation. No tegrastats subprocess or hardware writes.

## Scope and behavior

Add an opt-in `sensors` sampler on Linux. Discover thermal zones, hwmon channels,
and cooling devices using their native names and resolved device identity.
Read values on a configurable interval (default 5 seconds), in one nonoverlapping
blocking worker dispatched by refresh. Rediscover every 60 seconds in that worker;
cache metadata between discovery passes. Initialization performs no device reads.
Non-Linux builds register the metric schema without a sampler.

Metrics: `sensor_temperature` (millidegrees Celsius), `sensor_power` (microwatts),
`sensor_voltage` (millivolts; bus voltage only), `sensor_current` (milliamps),
`sensor_fan_speed` (RPM), `sensor_fan_pwm` (0–255), and `sensor_cooling_state`
(driver-defined state index). Each family has its own acquisition group and
reads family-major, with stamp-last and discard on wholly failed reads.
Failed or removed readings become absent, never manufactured zero.

Each descriptor carries `sensor` (unique identity within family), `source`,
`chip`, `channel`, optional native `label`, and optional verified `scope`.
Record board model and SoC compatibility as static context when available.
Unknown labels remain useful without invented component associations.
An identity change gets a fresh slot; never reuse or relabel a slot in this
process, avoiding recorder slot-label generation ambiguity. Limit each family
to 256 lifetime identities and report overflow on change. Rediscovery can
restore an identical sensor, but a replacement with different metadata is new.

Decode standard hwmon temperature, direct power, bus voltage/current and fan
channels; accept `rpm` only for NVIDIA `pwm_tach`. Respect channel enable/fault
attributes when exposed. INA3221 only pairs bus `in1..3_input` with corresponding
`curr1..3_input` for derived power using checked multiplication; do not mistake
its shunt voltages or summation channels for bus readings. Prefer direct power
when available. Mark derived power in metadata; sequential reads are not an
atomic electrical sample. Ignore NC/unconnected channels.

For the observed AGX Thor devkit, annotate INA238 at the documented device as
system power and preserve INA3221's native rail labels. No generic sum across
rails, no assumption that module input equals carrier-plus-module power.
Only verified board mapping permits scope annotations; all other boards get
native data. RPM is separate from PWM; cooling state is not a throttling percent.

## Presentation and validation

Add a Sensors dashboard with conditional charts for every metric family,
one line per sensor; scale temperature to Celsius and electrical readings to
W/V/A. Document missing permissions and unavailable sensors as absence.

Use the supplied Thor inventory as a filesystem fixture; add synthetic Orin
layouts explicitly labeled synthetic, failure/malformed/disabled cases,
rediscovery and identity-retention tests, cadence/window tests, and dashboard
query tests. Measure local dispatch and read cost separately, without claiming
local regular files or x86 sensors validate Thor hardware performance.

Thor live timing, reboot/driver reload, Orin hardware and alternate JetPack
versions remain validation requirements before fleet-wide enablement. No
automatic GitHub publication. Ship a read-only inventory/measurement recipe.

## Deliberate limits

No NVML changes or duplicate-source reconciliation across existing samplers.
No NIC classification without device evidence. Configurable limits, thermal
trip points, modes and overcurrent event counters are not collected in this
first measurement set; recordings do not explain those configuration changes.
Capabilities and identity are refreshed at discovery cadence; a change between
passes may take up to 60 seconds to be recognized. System calls can stall a
worker; latch prevents worker accumulation and overdue work is diagnosed.
