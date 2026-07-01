# Rezolus

**High-resolution, low-overhead performance telemetry for Linux systems, GPUs,
and the services running on top.**

Rezolus is a telemetry agent that captures detailed performance data across your
whole stack — kernel, CPU, GPU, and your services — as full distributions rather
than coarse averages. It uses eBPF, perf events, and NVIDIA NVML/GPM to reveal
the high-resolution behavior that traditional per-minute or per-second metrics
miss, then lets you export, record, replay, and analyze it.

> **Quick mental model:** Rezolus collects (**Agent**), exposes (**Exporter**),
> captures after the fact (**Hindsight**), records to disk (**Recorder**), and
> explores (**Viewer**) — all from a single binary.

### What you can do with Rezolus

![What you can do with Rezolus: rezolus agent collects from your systems (CPU, GPU, kernel, containers, services). From there you can expose Prometheus metrics with rezolus exporter, watch a live dashboard with rezolus view, or capture a recording with rezolus record or rezolus hindsight. Recordings are .parquet files you can explore in the viewer, analyze with an AI assistant via rezolus mcp, or manage with rezolus parquet.](docs/architecture.svg)

<sub>Every box is the same `rezolus` binary — you just pick the subcommand for the job. Source: [`docs/architecture.dot`](docs/architecture.dot) (regenerate with `dot -Tsvg docs/architecture.dot -o docs/architecture.svg`).</sub>

---

## Why Rezolus?

Most monitoring tools average performance data into per-second or per-minute
points. That smooths away exactly the behavior that matters: the latency tail, a
brief throttling event, a burst of retransmits, a scheduler stall. Rezolus is
built to keep that detail.

- **Distributions, not averages.** Latency, sizes, and utilization are captured
  as high-resolution histograms in the kernel, so you see the full shape — p50,
  p99, p99.9, and the long tail — not just a mean.
- **Low enough overhead to leave on.** eBPF samplers aggregate in-kernel and are
  read over a shared-memory map, so Rezolus is designed to run always-on,
  fleet-wide, in production. See [`docs/principles.md`](docs/principles.md) for
  the design rules the BPF samplers commit to.
- **Go back in time.** When per-second collection is too expensive but you still
  need fine-grained data, **Hindsight** keeps a high-resolution ring buffer on
  disk so you can snapshot system state *after* an incident has already
  happened.
- **Your data stays yours.** The Viewer runs its PromQL engine in your browser
  (compiled to WebAssembly), so recordings never have to leave your machine.

---

## Features

- **Systems telemetry via eBPF** — CPU usage, scheduler runqueue latency,
  syscall latency and counts, block I/O, TCP/network internals, and more,
  captured as high-resolution histograms instead of averages.
- **Rich CPU performance counters** — IPC (cycles and instructions), branch
  prediction, DTLB and L3 cache behavior, TLB flushes, migrations, and
  frequency, per core and per cgroup.
- **GPU telemetry** — NVIDIA via NVML and GPM, including per-tensor-pipe
  utilization (HMMA for FP16/BF16, IMMA for INT8, DFMA for FP64) on Hopper and
  newer, plus SM utilization/occupancy, DRAM bandwidth, PCIe throughput, power,
  energy, clocks, and temperature. Apple GPU metrics on macOS.
- **Container-aware** — per-cgroup CPU cycles/instructions, migrations, syscalls,
  and CFS bandwidth/throttling, so you can attribute behavior per container.
- **Service & inference telemetry** — runtime-loaded templates that turn
  service metrics into KPI dashboards, including vLLM (prefill / decode), SGLang
  (router / prefill / decode), and Valkey.
- **Hindsight** — an always-on, on-disk ring buffer that lets you snapshot
  high-resolution state *after* an incident has already happened.
- **Recorder** — capture metric snapshots to Parquet for benchmarking,
  workload characterization, and offline analysis.
- **Viewer** — an interactive web dashboard with in-browser PromQL, live
  streaming from an agent, and A/B compare mode with diff and quantile heatmaps.
- **MCP server** — LLM-guided analysis: query recordings with PromQL, detect
  anomalies, and analyze metric correlations from an AI assistant.
- **Integrates with your stack** — Prometheus-compatible export from the
  Exporter, and Parquet for portable storage and offline analysis.

### Capability matrix

| Domain | Sources | Notes |
|---|---|---|
| CPU / scheduler / syscalls | eBPF, perf events | Per-cgroup; latency histograms |
| CPU performance counters | perf events | IPC, branch, DTLB, L3, TLB flush, frequency |
| Block I/O | eBPF | Latency and size distributions by operation |
| Network / TCP | eBPF, ethtool/sysfs | Throughput, retransmits, connect/packet latency |
| Memory | meminfo, vmstat | System memory and paging activity |
| GPU (NVIDIA) | NVML, GPM | Per-pipe tensor utilization on Hopper+; PCIe, energy |
| GPU (Apple) | macOS sampler | Apple GPU metrics |
| Services / inference | Service templates | vLLM, SGLang, Valkey |

> **Platform requirements:** eBPF samplers require Linux kernel 5.8+ and root,
> on x86_64 or ARM64. macOS support is limited to basic CPU and Apple GPU
> metrics — use the Viewer and Recorder to work with remote Linux agents.

See the [metrics documentation][metrics] for the full list of metrics Rezolus
supports.

---

## Quick Start

```bash
git clone https://github.com/iopsystems/rezolus
cd rezolus
cargo build --release
```

Capture system metrics for 60 seconds and view them in your browser:

```bash
sudo scripts/rezolus-capture --duration 60s
```

The script starts the Rezolus agent automatically, records system metrics, and
launches an interactive dashboard. Root privileges are required for eBPF
instrumentation.

To also capture service metrics from a Prometheus-compatible endpoint (e.g.,
Valkey via [redis_exporter][redis_exporter]):

```bash
sudo scripts/rezolus-capture --duration 2m \
    --endpoint http://localhost:9121/metrics \
    --source valkey
```

Run `scripts/rezolus-capture --help` for all options.

### Docker

A Docker image is also available for trying Rezolus without installing from
source:

```bash
docker run --rm -it --privileged \
  -p 8080:8080 -v $(pwd)/data:/data \
  ghcr.io/iopsystems/rezolus:latest \
  rezolus-capture --duration 60s
```

See [docker/README.md](docker/README.md) for more examples including combined
system + service metric captures.

---

## Architecture

Rezolus ships as a single binary that runs in several roles. The first three run
as managed services; the rest are on-demand subcommands.

### Agent

The core component. It collects performance metrics from the system using eBPF,
perf events, NVML/GPM, and traditional sources, and serves them over HTTP. The
agent listens on `0.0.0.0:4241` by default, so the Exporter, Recorder, and Viewer
can all read from it — locally or across the network.

Individual samplers can be enabled, disabled, or retuned in the agent config.

```bash
# edit the agent config
sudo editor /etc/rezolus/agent.toml
# restart to apply
sudo systemctl restart rezolus
```

### Exporter

Transforms collected metrics for Prometheus compatibility and exposes them on a
Prometheus-compatible endpoint. It can summarize histogram distributions down to
a few percentiles to cut storage cost, or expose full histogram buckets when you
need them.

Set the exporter interval to match your scrape interval: too short and summary
metrics won't cover the gap between scrapes; too long and metrics go stale.

```bash
sudo editor /etc/rezolus/exporter.toml
sudo systemctl restart rezolus-exporter
```

### Hindsight

Sometimes per-second collection is too expensive, and some problems are
impossible to understand without fine-grained data. Hindsight keeps a
high-resolution ring buffer on disk so you can record a snapshot *after* a
problem has already occurred — effectively going back in time to root-cause a
production incident at full resolution.

Hindsight is **disabled by default**. Review the config before enabling it.

```bash
sudo editor /etc/rezolus/hindsight.toml
sudo systemctl enable rezolus-hindsight
sudo systemctl start rezolus-hindsight
# trigger a save of the ring buffer to the output file
sudo systemctl kill -sHUP rezolus-hindsight
```

Hindsight can also expose an optional HTTP endpoint for remote buffer
management — see [HTTP Endpoint](#http-endpoint-optional) below.

### Recorder

Connects to a running agent and records metrics into a Parquet file for
benchmarking, lab tests, or offline workload characterization. It auto-detects
Rezolus agent vs Prometheus sources and supports custom file-level metadata.

```bash
rezolus record --interval 1s --duration 15m http://localhost:4241 rezolus.parquet
```

### Viewer

An interactive web dashboard for exploring recordings or streaming live from a
running agent. PromQL runs locally in the browser (compiled to WebAssembly), so
your data stays on your machine. It supports live mode, A/B compare with diff
heatmaps, and quantile heatmaps. Because the agent listens on `0.0.0.0:4241`,
you can point the Viewer at a remote host.

```bash
# open a recording
rezolus view rezolus.parquet
# A/B compare two recordings
rezolus view baseline.parquet experiment.parquet
# stream live from an agent
rezolus view http://localhost:4241
# upload-only mode (no file argument)
rezolus view
```

The same dashboard is also available as a browser-only static site under
[`site/viewer/`](site/viewer/), powered by the
[`crates/viewer`](crates/viewer) WASM module. It runs the PromQL query engine
client-side, so parquet files never leave the browser.

### Parquet Tools

File operations for parquet recordings:

- **Metadata** — inspect file-level and column-level metadata, geometry, and
  schema.
- **Annotate** — embed service extension KPI definitions for custom viewer
  dashboards.
- **Combine** — merge a Rezolus parquet with service-level parquet files,
  joining on timestamps to produce a unified multi-source recording.

```bash
rezolus parquet metadata -i rezolus.parquet
rezolus parquet annotate rezolus.parquet --queries ext.json
rezolus parquet combine rezolus.parquet service.parquet -o combined.parquet
```

### MCP Server

Exposes Rezolus recordings to LLM-based assistants over the Model Context
Protocol, with tools for querying metrics via PromQL, detecting anomalies, and
analyzing correlations — useful for AI-guided performance investigation. Runs as
a stdio MCP server or as one-shot CLI commands.

```bash
rezolus mcp                                                  # stdio server
rezolus mcp detect-anomalies rezolus.parquet                 # anomaly detection
rezolus mcp query rezolus.parquet "sum(rate(cpu_cycles[1m]))"
```

---

## Use Cases

### Performance engineering

Run just the Agent and use the Recorder to take on-demand captures during tests
in lab environments, or capture production performance data to characterize a
workload and understand what conditions you'd want to replicate in test.

Collect a per-second recording for 15 minutes, then open it:

```bash
rezolus record --interval 1s --duration 15m http://localhost:4241 rezolus.parquet
rezolus view rezolus.parquet
```

### DevOps and SRE troubleshooting

Run the Agent and Exporter to integrate Rezolus telemetry with your Prometheus
stack and get deeper insight into production behavior. The Exporter can summarize
histograms down to a few percentiles, greatly reducing the storage cost of
distribution-aware metrics.

When per-second collection is too expensive and a problem is hard to understand
without fine-grained data, enable **Hindsight**: its on-disk ring buffer lets you
dump a high-resolution snapshot *after* an incident, so you can go back in time
and root-cause the issue at full resolution.

### AI inference and services

Capture Rezolus system telemetry alongside service metrics from inference
servers and datastores. Runtime-loaded templates for vLLM (prefill / decode),
SGLang (router / prefill / decode), and Valkey turn those metrics into KPI
dashboards in the Viewer, so you can correlate model-serving behavior with
kernel, CPU, and GPU activity.

---

## Installation

### Quick install (recommended)

```bash
curl -fsSL https://install.rezolus.com | bash
```

The quick install script works on both Linux and macOS. On macOS it uses
Homebrew if available, or falls back to Cargo. It adds the package repo,
installs Rezolus, and starts the agent and exporter as systemd services.
Supported distributions include Debian, Ubuntu, Rocky Linux, and Amazon Linux.

By default, the `rezolus` (agent) and `rezolus-exporter` services run after
install, so Prometheus exposition is available immediately. The config assumes
per-second collection — review it and adjust as needed for your environment. The
`rezolus-hindsight` service is disabled by default; review its config before
enabling it.

For detailed instructions, see the [Installation Guide](docs/installation.md).

### Build from source

Rezolus is built with the standard Rust toolchain (install via
[rustup](https://rustup.rs/)).

```bash
git clone https://github.com/iopsystems/rezolus
cd rezolus
cargo build --release

# run the agent
sudo target/release/rezolus config/agent.toml

# run the exporter
sudo target/release/rezolus exporter config/exporter.toml

# record metrics to a parquet file
target/release/rezolus record http://localhost:4241 rezolus.parquet

# run hindsight
target/release/rezolus hindsight config/hindsight.toml

# view a recording (or connect to a live agent)
target/release/rezolus view rezolus.parquet
target/release/rezolus view http://localhost:4241

# parquet file operations
target/release/rezolus parquet metadata -i rezolus.parquet
target/release/rezolus parquet combine rezolus.parquet service.parquet -o combined.parquet
```

To rebuild the browser-only static viewer (`site/viewer/`) that ships the PromQL
engine as WebAssembly, install
[wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) (0.13+) and run:

```bash
./crates/viewer/build.sh
```

The artifacts land in `site/viewer/pkg/`. See
[`crates/viewer/README.md`](crates/viewer/README.md) for details.

---

## Configuration

Rezolus has three services, each with its own configuration file in
`/etc/rezolus/`:

| Service | Config | Default |
|---|---|---|
| `rezolus` (agent) | `agent.toml` | enabled |
| `rezolus-exporter` | `exporter.toml` | enabled |
| `rezolus-hindsight` | `hindsight.toml` | disabled |

Each sampler can be individually enabled or disabled, and its collection
interval adjusted, in the [agent config][agent.toml]. The
[exporter config][exporter.toml] **must** set its `interval` to match your
Prometheus scrape interval, and can optionally expose full histograms instead of
summary percentiles. Review the [hindsight config][hindsight.toml] before
enabling that service.

### HTTP Endpoint (Optional)

Hindsight can optionally expose an HTTP endpoint for remote buffer management.
Enable it by adding a `listen` address to the configuration:

```toml
listen = "127.0.0.1:4242"
```

Available endpoints:

- `GET /status` — returns buffer status including time range, utilization, and
  snapshot count
- `GET /dump` — downloads the ring buffer as a parquet file
- `POST /dump/file` — writes the ring buffer to the configured output file

The `/dump` and `/dump/file` endpoints support query parameters for time
filtering:

| Parameter | Description                             | Example                       |
| --------- | --------------------------------------- | ----------------------------- |
| `last`    | Relative time range                     | `?last=5m`                    |
| `start`   | Start time (Unix timestamp or RFC 3339) | `?start=2024-01-01T12:00:00Z` |
| `end`     | End time (Unix timestamp or RFC 3339)   | `?end=2024-01-01T13:00:00Z`   |

Examples:

```bash
# check buffer status
curl http://localhost:4242/status

# download last 5 minutes as parquet
curl -o dump.parquet "http://localhost:4242/dump?last=5m"

# download a specific time range using RFC 3339 datetime
curl -o dump.parquet "http://localhost:4242/dump?start=2024-01-01T12:00:00Z&end=2024-01-01T13:00:00Z"

# trigger a dump to the configured output file
curl -X POST http://localhost:4242/dump/file
```

---

## Design Principles

Rezolus's BPF samplers follow a specific set of design principles around
overhead, kernel compatibility, and the always-on production deployment model.
See [`docs/principles.md`](docs/principles.md) for the full list, the operational
checklist used when reviewing or writing a sampler, and the current improvement
backlog.

---

## Deployment

- **Architectures:** x86_64 and ARM64
- **Deployment:** bare-metal and cloud environments
- **Linux kernel:** 5.8+ for eBPF samplers

---

## Community & Support

- [Discord Community][discord]
- [GitHub Issues][new issue]

---

## Contributing

To contribute, first check whether an open issue or pull request already covers
the bug or feature you have in mind. If not, please open a
[new issue on GitHub][new issue] to report the bug or get feedback on a proposed
feature before starting work. This lets a maintainer confirm the bug and provide
early input on new features.

Once you're ready to contribute, the workflow is:

- [create a fork][create a fork] of this repository
- clone your fork and create a new feature branch
- make your changes and write a helpful commit message
- push your feature branch to your fork
- open a [new pull request][new pull request]

To develop new samplers and get the best experience, build and run on Linux.

---

## License

Dual-licensed under [Apache 2.0][license apache] and [MIT][license mit], unless
otherwise specified. Detailed licensing information can be found in the
[COPYRIGHT][copyright] file.

[agent.toml]: https://github.com/iopsystems/rezolus/blob/main/config/agent.toml
[copyright]: https://github.com/iopsystems/rezolus/blob/main/COPYRIGHT
[create a fork]: https://github.com/iopsystems/rezolus/fork
[discord]: https://discord.gg/YC5GDsH4dG
[exporter.toml]: https://github.com/iopsystems/rezolus/blob/main/config/exporter.toml
[hindsight.toml]: https://github.com/iopsystems/rezolus/blob/main/config/hindsight.toml
[license apache]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-APACHE
[license mit]: https://github.com/iopsystems/rezolus/blob/main/LICENSE-MIT
[metrics]: https://github.com/iopsystems/rezolus/blob/main/docs/metrics.md
[new issue]: https://github.com/iopsystems/rezolus/issues/new
[new pull request]: https://github.com/iopsystems/rezolus/compare
[redis_exporter]: https://github.com/oliver006/redis_exporter
