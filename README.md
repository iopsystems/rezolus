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

### Hindsight
Provides after-the-fact artifacts for incident investigation:
- Maintains a rolling, high-resolution metrics buffer
- Snapshot metrics during or after performance incidents
- Capture detailed system state when unexpected events occur

## Use Cases
We believe that Rezolus is useful for:
- Performance engineering
- DevOps and SRE troubleshooting

### Performance Engineering
Rezolus can be run with just the Agent and the Recorder can be used to take
on-demand captures during tests run in lab environments or to capture production
performance data to help characterize workload and understand what conditions
you may want to replicate in test environments.

Simply run the following command to collect a secondly recording for 15 minutes:
```bash
rezolus record --interval 1s --duration 15m http://localhost:4241 rezolus.parquet
```

### DevOps and SRE Troubleshooting
Rezolus also has value for people operating services. The Agent and Exporter can
be used to integrate Rezolus telemetry with your observability stack and give
deeper insights into production behaviors. The Exporter is designed to allow
summarization of histograms to just a few percentiles, which greatly reduces the
storage requirements to get insights around distributions.

Unfortunately, sometimes it is too expensive to collect telemetry on a secondly
basis. And some problems are very difficult to understand without fine-grained
metrics. This is exactly what the Rezolus Hindsight is designed for. By keeping
a high-resolution ring buffer on disk, you can record a snapshot to disk after a
problem has already happened! Imagine being able to go back in time and get that
high-resolution data to root cause a production performance issue. With Rezolus
Hindsight, you can do exactly that.

## Community & Support
- [Discord Community][discord]
- [GitHub Issues][new issue]

## License
Dual-licensed under [Apache 2.0][license apache] and [MIT][license mit], unless
otherwise specified.

Detailed licensing information can be found in the [COPYRIGHT][copyright] file.

## Deployment

### Supported Environments
- Architectures: x86_64 and ARM64
- Deployment: Bare-metal and cloud environments
- Linux kernel 5.8+

### Install
Find an appropriate package for your OS for our [latest release][latest release]
and install it using your package manager.

By default the `rezolus` service will be running as the agent and the
`rezolus-exporter` service will be running so there is Prometheus exposition. By
default, the config assumes secondly collection. Please review the config and
adjust as necessary for your environment.

The `rezolus-hindsight` service is disabled by default. Please review the config
before enabling.

```bash
# enable and start the service
systemctl enable rezolus-hindsight
systemctl start rezolus-hindsight
```

### Configuration
Rezolus has three services each with its own configuration.

#### Agent
The agent config may be adjusted to enable/disable individual samplers.

Please see the [config/agent.toml][agent.toml] to review all configuration
options and their defaults.

```bash
# edit the agent config file
editor /etc/rezolus/agent.toml

# restart the service to apply changes
systemctl restart rezolus
```

#### Exporter
The exporter **must** be configured so that the `interval` matches the scrape
interval for metrics in your environment. If the interval is too short, any
summary metrics will not cover the entire period between scrapes of the metrics
endpoint. Setting it too long will cause stale metrics to be served.

Additionally, the exporter may be configured to expose full histograms instead
of summary percentiles.

Please see the [config/exporter.toml][exporter.toml] to review all configuration
options and their defaults.

```bash
# edit the exporter config file
sudo editor /etc/rezolus/exporter.toml

# restart the exporter to apply changes
sudo systemsctl restart rezolus-exporter
```

#### Hindsight
This service is disabled by default. Please review the configuration and make
any necessary changes before you enable it.

Please see the [config/hindsight.toml][hindsight.toml] to review all
configuration options and their defaults.

```bash
# review the config file and make any desired changes
sudo editor /etc/rezolus/hindsight.toml

# enable and start the service
sudo systemctl enable rezolus-hindsight
sudo systemctl start rezolus-hindsight

# trigger a save of the ring buffer to the output file
sudo systemctl kill -sHUP rezolus-hindsight
```

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

# to run the hindsight
target/release/rezolus hindsight config/hindsight.toml
```

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
