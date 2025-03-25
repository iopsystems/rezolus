# Rezolus: High-Resolution Systems Performance Telemetry

## What is Rezolus?

Rezolus is a Linux performance telemetry agent that provides detailed insights
into system behavior through efficient, low-overhead instrumentation.

## Performance Metrics

Rezolus captures a comprehensive set of system performance metrics across
multiple domains:

- **CPU**: Measure utilization and performance metrics
- **Scheduler**: Probe task execution and system responsiveness
- **Block IO**: Analyze workload characteristics and performance
- **Network**: Explore traffic and protocol dynamics
- **System Calls**: Examine invocation patterns and latencies
- **Container-level**: Quantify container-level performance dynamics

By using eBPF, Rezolus provides high-resolution, low-overhead instrumentation
that reveals detailed system behavior.

## Operating Modes

### Agent
The core component of Rezolus that collects performance metrics from the system.
It provides the foundational telemetry gathering capabilities.

### Exporter
Transforms collected metrics for Prometheus compatibility:
- Exposes metrics on a Prometheus-compatible endpoint
- Allows conversion of histogram distributions to summary metrics

### Recorder
Enables on-demand metric collection:
- Write metrics directly to file
- Flexible, targeted performance analysis

### Flight Recorder
Provides artifacts for incident investigation:
- Maintains a rolling, high-resolution metrics buffer
- Snapshot metrics during or after performance incidents
- Capture detailed system state when unexpected events occur

## Deployment

### Supported Environments
- Architectures: x86_64 and ARM64
- Deployment: Bare-metal and cloud environments
- Linux kernel 5.8+

### Install
Find an appropriate package for your OS for our [latest release][latest release]
and install it using your package manager.

By default the `rezolus` service will be running as the agent and the
`rezolus-exporter` service will be running so there is prometheus exposition. To
enable the flight-recorder, you can do:

```bash
systemctl enable rezolus-flight-recorder
systemctl start rezolus-flight-recorder
```

The flight-recorder can be configured in the service file which will be located
at `/etc/systemd/system/rezolus-flight-recorder.service`.

### Build from source
```bash
git clone https://github.com/iopsystems/rezolus
cd rezolus
cargo build --release

# to run the agent
sudo target/release/rezolus config/agent.toml

# to run the exporter
sudo target/release/rezolus exporter config/exporter.toml

# to record
target/release/rezolus record http://localhost:4241 rezolus.parquet

# to run the flight recorder
target/release/rezolus flight-recorder http://localhost:4241 rezolus.parquet
```

## Use Cases
- Performance engineering
- System behavior analysis
- DevOps and SRE troubleshooting

## Community & Support
- [Discord Community][discore]
- [GitHub Issues][new issue]

## License
Dual-licensed under [Apache 2.0][license apache] and [MIT][license mit], unless
otherwise specified.

Detailed licensing information can be found in the [COPYRIGHT][copyright] file.

## Contributing
To contribute to Rezolus first check if there are any open pull requests or
issues related to the bugfix or feature you wish to contribute. If there is not,
please start by opening a [new issue on GitHub][new issue] to either report the
bug or get feedback on a new feature. This will allow one of the maintainers to
confirm the bug and provide early input on new features.

Once you're ready to contribute some changes, the workflow is:
* [create a fork][create a fork] of this repository
* clone your fork and create a new feature branch
* make your changes and write a helpful commit message
* push your feature branch to your fork
* open a [new pull request][new pull request]

[copyright]: https://github.com/iopsystems/rezolus/blob/main/COPYRIGHT
[create a fork]: https://github.com/iopsystems/rpc-perf/fork
[discord]: https://discord.gg/YC5GDsH4dG
[latest release]: https://github.com/iopsystems/rezolus/releases/latest
[license apache]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-APACHE
[license mit]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-MIT
[new issue]: https://github.com/iopsystems/rezolus/issues/new
[new pull request]: https://github.com/iopsystems/rpc-perf/compare
[rust-lang.org]: https://www.rust-lang.org/
