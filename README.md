# Rezolus

**High-resolution systems telemetry for tail latency, missed deadlines, and
performance regressions.**

Rezolus measures CPU, GPU, scheduler, I/O, and network behavior so you can
investigate what the machine was doing when a workload slowed down. It preserves
latency distributions, records telemetry for later analysis, and provides live
and offline dashboards—all from one binary.

Use it for performance engineering, production incidents, inference serving,
and latency-critical workloads such as physical AI, trading, and storage.
It complements application metrics, tracing, and profiling: Rezolus supplies
system-level evidence, not individual request traces or call-stack profiles.

[Quick start](#quick-start) · [Choose a workflow](#choose-a-workflow) ·
[Metrics](docs/metrics.md) · [Documentation map](#documentation-map)

**See it before installing:** [explore the sample recording in the web viewer](https://rezolus.com/viewer/?capture=demo.parquet).
The sample loads automatically and is processed in your browser. Explore the
scheduler and I/O histograms alongside CPU activity; no installation or file
upload is needed.

## Why Rezolus?

- **See the tail, not just the average.** Scheduler, syscall, and I/O latency
  histograms preserve the distribution of events between reads. A short delay
  can appear in the distribution even when you collect snapshots once a second.
  Counters and gauges retain their own types; not every metric is a histogram.
- **Choose the time detail you need.** Record at one-second intervals for a
  longer observation or shorten the interval for a focused experiment. Event
  duration resolution and snapshot cadence are different: reading more often
  improves when you can locate a change, not the precision of each event's
  duration. Agent caching and individual sampler cadences also limit freshness.
- **Keep evidence from before an incident.** Hindsight continuously writes a
  local rolling buffer. Save the recent history after a trigger instead of
  trying to reproduce the event. It must be running beforehand and needs disk
  space; it avoids indefinite retention, not the cost of collection.
- **Collect continuously, investigate on demand.** eBPF samplers aggregate in
  the kernel and use memory-mapped reads. Overhead depends on enabled samplers,
  event rates, hardware, and collection cadence. The [design principles](docs/principles.md)
  describe the cost constraints and measurement requirements; validate them on
  your workload before fleet-wide deployment.
- **Use the same evidence in several ways.** Inspect a recording in the web
  viewer or terminal, compare baseline and experiment, query it through MCP,
  or export metrics to Prometheus. No hosted telemetry service is required.

## Quick start

### Linux: install, capture, view

For supported Debian, Ubuntu, Enterprise Linux, and Amazon Linux systems with
systemd:

```bash
curl -fsSL https://install.rezolus.com | sudo bash
```

The installer asks whether to enable continuous collection. Accepting starts
the Agent and Exporter; Hindsight remains disabled. For unattended installation,
use `sudo bash -s -- -y`; add `--disable-services` to install without starting
collection. See the [installation guide](docs/installation.md) for supported
releases, package repositories, and source builds.

With the local agent running, capture a minute and open it:

```bash
rezolus record --duration 60s -o first-run.rez
rezolus view first-run.rez
```

The recorder defaults to `http://localhost:4241`. It does **not** start an agent.
Choose a new output filename for each `.rez` capture; existing files are not
overwritten. The viewer prints its local address for your browser. To watch the
agent without recording:

```bash
rezolus view http://localhost:4241
```

**Collection requirements:** Linux kernel 5.8+ with usable BTF and root/sudo for
eBPF instrumentation; x86_64 or ARM64. Sampler availability depends on kernel,
hardware, drivers, and permissions. The agent listens on `0.0.0.0:4241` by
default; use `127.0.0.1:4241` in its configuration for local-only access.

### macOS or an analysis workstation

```bash
brew install iopsystems/iop/rezolus
rezolus view first-run.rez
# Or inspect a reachable Linux agent:
rezolus view http://linux-host:4241
```

Viewing and analyzing an existing recording does not require Linux eBPF or
root. Local collection on macOS is limited to CPU and Apple GPU metrics;
it does not provide Linux scheduler, syscall, or block-I/O instrumentation.
The macOS installer does not install Linux systemd services.

[Build from source](docs/installation.md#building-from-source) ·
[Docker](docker/README.md) · [Troubleshooting](docs/troubleshooting.md)

## Choose a workflow

| You want to… | Start here | What it needs / produces |
| --- | --- | --- |
| See the machine now | `rezolus view http://host:4241` | A reachable agent; live dashboard. Add `--tui` for a terminal view. |
| Capture a benchmark | `rezolus record -o run.rez -- ./benchmark` | An existing agent; records for the command's lifetime. Then `rezolus view run.rez`. |
| Explain a past incident | [Hindsight](docs/usage.md#hindsight) | A buffer running before the incident; save recent history to a recording. |
| Feed Prometheus | [Exporter](docs/usage.md#exporter) | A running agent; set the exporter interval to match the scrape interval. |
| Compare a change | `rezolus view baseline.rez experiment.rez` | Two recordings; browser A/B comparison. |
| Capture service and system metrics together | [Multiple endpoints](docs/usage.md#several-endpoints-in-one-rez) | Agent and/or Prometheus endpoints; one recording per endpoint in a `.rez` archive. |
| Investigate with an AI assistant | [MCP workflow](#analyze-with-an-agent) | A recording; discovery, feature extraction, and targeted queries. |
| Inspect or transform captures | `rezolus recording --help` | [Recording tools](docs/usage.md#recording-tools) for metadata, annotation, combination, filtering, conversion, and snapshots. |

For example, record at 100 ms while a benchmark runs:

```bash
rezolus record --interval 100ms -o benchmark.rez -- ./my-benchmark --threads 8
rezolus view benchmark.rez
```

System and service endpoints can also share an archive:

```bash
rezolus record --duration 60s \
  --endpoint http://localhost:4241,source=system \
  --endpoint http://localhost:9121/metrics,source=valkey \
  -o combined.rez
```

Each endpoint remains a distinct recording. KPI templates for vLLM, SGLang, and
Valkey provide service-specific dashboards; they consume exposed service metrics,
not automatically instrument application code. See [KPI dashboard setup](docs/parquet_metadata.md#service_queries)
for annotation and template instructions (`.rez` archives require an explicit
`--queries` file), and the [usage guide](docs/usage.md) for recording labels and selection.

## What Rezolus measures

| Area | Examples | Availability |
| --- | --- | --- |
| CPU and scheduler | Usage, runqueue latency, cycles/instructions, cache/TLB behavior, migrations, frequency | Linux; PMU counters depend on hardware and available counter slots |
| I/O and networking | Block-I/O latency and sizes, TCP behavior, interface statistics, syscall latency/counts | Linux; individual samplers depend on kernel support |
| Containers | Cgroup CPU usage and counters, migrations, syscalls, bandwidth/throttling | Linux cgroups; coverage varies by metric |
| GPUs | Utilization, memory activity, clocks, power, temperature, and hardware counters | NVIDIA NVML/GPM, AMD SMI/PMU, Intel i915 PMU, Apple GPU; metric sets differ |
| Memory and capacity | Memory/NUMA statistics, local filesystem occupancy, drive health | Platform and device dependent |
| Services | Inference and datastore KPIs alongside system telemetry | Prometheus-compatible metrics and service templates |

Consult the [metric definitions](docs/metrics.md), [sampler configuration](config/agent.toml),
and GPU-specific guides before assuming a signal exists. A disabled or
unsupported sampler is not evidence of zero activity.

## How the pieces fit

![Rezolus Agent collects system telemetry. Exporter exposes it to Prometheus; Viewer watches it live; Recorder and Hindsight save recordings. Viewer, MCP, and recording tools work with saved data. Services can also supply Prometheus metrics directly to Recorder.](docs/architecture.svg)

One `rezolus` binary supplies these roles. The Agent, Exporter, and Hindsight
can run as managed services; Recorder, Viewer, MCP, and recording tools run on
demand. Solid boxes in the diagram are Rezolus commands; dashed boxes are the
systems and services being measured.

The normal capture format is **`.rez`**, an archive with a recording per endpoint
and separate acquisition groups/cadences. It accepts Rezolus and Prometheus
sources. Use **`.parquet`** for a uniform tabular export or other Parquet tools;
**`.raw`** captures snapshots for later conversion. See [format details](docs/usage.md#output-formats)
for the tradeoffs and overwrite rules.

`rezolus view` runs a local Rust server that evaluates queries and serves the
web dashboard. The [browser-only viewer](https://rezolus.com/viewer/) instead
uses WebAssembly to process uploaded recordings entirely in the browser.
Both can inspect `.rez` and `.parquet` recordings; the CLI viewer also connects
to live agents.

## Analyze with an agent

An assistant can launch `rezolus mcp` as a stdio MCP server or invoke the same
capabilities as one-shot CLI commands. Start by discovering what is actually
in the recording:

```bash
rezolus mcp describe-recording run.rez
rezolus mcp describe-metrics run.rez
rezolus mcp extract-features run.rez
```

`extract-features` produces versioned JSON with summary statistics, anomalies,
regime shifts, correlations, resource rankings, and coverage. It requires at
least 10 seconds of data. Then query a specific hypothesis using the metric
names, types, and labels returned by `describe-metrics`:

```bash
rezolus mcp query run.rez 'sum(rate(cpu_cycles[1m]))'
```

For a multi-recording archive, first run `describe-recording` without a selector
to list the recordings, then pass `--recording source=system` (or another
matching label set) to subsequent commands. MCP tools take the equivalent
`recording` object. A selector must identify exactly one recording.

Before drawing conclusions, check the time range, source identity, metric
coverage, units, and acquisition uncertainty. Missing data is not zero;
correlation is a lead to investigate, not proof of causation. See the
[MCP reference](docs/usage.md#mcp-server) for tool names and selection examples.

## Configuration

Linux packages place configuration under `/etc/rezolus/`:

| Service | Configuration | Installer default when services are enabled |
| --- | --- | --- |
| Agent (`rezolus`) | [agent.toml](config/agent.toml) | Running |
| Exporter (`rezolus-exporter`) | [exporter.toml](config/exporter.toml) | Running |
| Hindsight (`rezolus-hindsight`) | [hindsight.toml](config/hindsight.toml) | Disabled |

Enable or disable samplers in the agent config and restart the agent to apply
changes. Recorder/Hindsight intervals control how often they request samples;
`general.ttl` and slower sampler-specific intervals bound freshness. Match the
exporter's interval to your Prometheus scrape interval. Review Hindsight's
lookback and disk requirements before enabling it.

## Documentation map

| Task | Read next |
| --- | --- |
| Install packages, build from source, or check platform requirements | [Installation](docs/installation.md), [Docker](docker/README.md) |
| Record, compare, export, retain history, or analyze recordings | [Usage guide](docs/usage.md) |
| Find a metric's meaning, unit, and labels | [Metrics](docs/metrics.md) |
| Configure service KPI dashboards | [KPI definitions and annotation](docs/parquet_metadata.md#service_queries) |
| Send application metrics into the agent over a Unix socket | [External metrics](docs/external_metrics.md) |
| Check flags or HTTP endpoints | `rezolus <command> --help`, [CLI reference](https://rezolus.com/docs/cli.html), [HTTP API](https://rezolus.com/docs/api.html) |
| Diagnose installation or collection problems | [Troubleshooting](docs/troubleshooting.md) |
| Understand sampler cost and measurement constraints | [Design principles](docs/principles.md) |
| Change the implementation | [Contributor guidance](CLAUDE.md), [samplers](src/agent/samplers/), [shared dashboards](crates/dashboard/), [browser viewer](crates/viewer/), [recording format](crates/rez/) |

This README describes the checked-out revision. For installed releases, check
`rezolus --version` and that binary's help when a documented flag is unavailable.

## Contributing and support

Before starting a change, check existing issues and PRs. If none covers it,
[open an issue](https://github.com/iopsystems/rezolus/issues/new) to discuss the
bug or proposed feature. Fork the repository, create a branch, add relevant
tests, and open a PR. Develop Linux samplers on Linux and read the
[sampler principles](docs/principles.md) first.

Questions and discussions: [Discord](https://discord.gg/YC5GDsH4dG) ·
[GitHub issues](https://github.com/iopsystems/rezolus/issues)

## License

Dual-licensed under [Apache 2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT), unless
otherwise specified. See [COPYRIGHT](COPYRIGHT) for details.
