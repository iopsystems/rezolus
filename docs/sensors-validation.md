# Validate the hardware sensors sampler

The `sensors` sampler is opt-in. It reads Linux sysfs without changing hardware
settings. Its initial Tegra fixture comes from an AGX Thor developer kit running
L4T R39.2.1 and kernel `6.8.12-tegra-bpf`; fixture tests do not measure hardware
access cost or establish Orin hardware support.

## Inventory

Run directly on the host, outside containers that hide sysfs:

```sh
python3 scripts/sensor_inventory.py > sensors-inventory.json
```

The JSON preserves native names, resolved paths, platform identity and read
errors. This version includes NVIDIA's `rpm`, overcurrent event attributes,
conversion intervals and shunt configuration, which the first Thor inventory
omitted. It reads configuration but never writes it. Permission errors should
be investigated before deciding whether elevated privileges are necessary.

For a Tegra reference trace, optionally run:

```sh
sudo timeout 15s tegrastats --interval 1000 > tegrastats.txt
```

`timeout` normally exits 124 after stopping the trace. Preserve the board model,
L4T release and kernel version with each result. AGX Orin and Orin NX/Nano are
different fixture targets: their power domains are not interchangeable.

## Run the agent and measure

Build on the target architecture with the repository's normal dependencies:

```sh
cargo build --release --bin rezolus
cat > /tmp/rezolus-sensors.toml <<'EOF'
[general]
listen = "127.0.0.1:4299"
[scheduler]
policy = "normal"
[log]
level = "debug"
[defaults]
enabled = false
[samplers.sensors]
enabled = true
interval = "5s"
EOF
target/release/rezolus /tmp/rezolus-sensors.toml > /tmp/rezolus-sensors.log 2>&1 &
sensor_agent_pid=$!
```

Use a free local port if 4299 is occupied. Drive sampling for at least 90 seconds
to include rediscovery. The agent samples on demand; an idle process with no
scrapes or subscription does not provide a useful timing run.

```sh
for i in $(seq 1 90); do
    curl --fail --silent http://127.0.0.1:4299/metrics/json > /tmp/rezolus-sensors-latest.json
    sleep 1
done
kill "$sensor_agent_pid"
```

Keep the log, last JSON snapshot and inventory. Report cached-refresh latency,
dispatch latency, complete background sweep duration, discovery duration (where
reported), and sensor counts separately. A fast dispatch does not prove fast
hardware access. Repeat during representative activity; compare readings with
tegrastats without expecting separately timed samples to match exactly.

Check CPU/GPU/SoC temperature response, fan RPM versus PWM command, distinct
power rails, absent/error handling, and successful recovery after rediscovery.
After a reboot, sensor indices can change: names and device associations should
still explain every line in a new recording. Driver reload validation must be
done only on a host where disruption is acceptable; this recipe does not reload
drivers or change power, fan, thermal, or permission settings.

## Interpretation

- Temperature values use millidegrees Celsius; power uses microwatts; bus
  voltage uses millivolts; current uses milliamps. The viewer scales these.
- Derived INA3221 power multiplies bus voltage and current read sequentially.
  It is not an atomic electrical measurement. Do not sum nested rails or
  summation channels into an invented total.
- Thor's INA238 VIN covers module and carrier board. Orin NX/Nano VDD_IN is
  module power. A combined CPU/GPU rail cannot yield separate component power.
- PWM is the fan command, not evidence that the fan rotates. RPM is a separate
  reading. Cooling states are driver-defined indices, not percent throttled.
- Unlabeled external temperature channels are not automatically NIC sensors.
- Sysfs hwmon may expose the same temperature as a thermal zone or an existing
  GPU/drive sampler. Distinct source identities remain separate; do not add them.
- Limits, modes, thermal trip points, and overcurrent events in the inventory
  are diagnostic context; this first sampler does not record their time series.

References: [NVIDIA Thor power and thermal guide](https://docs.nvidia.com/jetson/archives/r38.2/DeveloperGuide/SD/PlatformPowerAndPerformance/JetsonThor.html),
[NVIDIA Orin guide](https://docs.nvidia.com/jetson/archives/r36.4.4/DeveloperGuide/SD/PlatformPowerAndPerformance/JetsonOrinNanoSeriesJetsonOrinNxSeriesAndJetsonAgxOrinSeries.html),
[INA3221 ABI](https://www.kernel.org/doc/html/next/hwmon/ina3221.html).
