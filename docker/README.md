# Rezolus Docker Image

A Docker image for trying out Rezolus -- capture high-resolution system and
service metrics, then view them in an interactive dashboard.

The container runs the Rezolus agent automatically for system metrics collection
(CPU, memory, network, disk, scheduler, syscalls, TCP) via eBPF. It also
includes a `rezolus-capture` script that records metrics for a given duration,
optionally combines system metrics with service metrics from a
Prometheus-compatible endpoint, and launches the Rezolus viewer.

## Quick Start

Build the image:

```bash
docker build -f docker/Dockerfile -t rezolus .
```

Or pull a pre-built image:

```bash
docker pull ghcr.io/iopsystems/rezolus:latest
```

## Usage

### System metrics only

Capture system metrics for 60 seconds and view the results:

```bash
docker run --rm -it --privileged \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  rezolus \
  rezolus-capture --duration 60s
```

Then open http://localhost:8080 in your browser.

### System + service metrics (e.g., Redis)

Start Redis and a Prometheus exporter for it:

```bash
docker run -d --name redis -p 6379:6379 redis:latest
docker run -d --name redis-exporter -p 9121:9121 \
  --link redis oliver006/redis_exporter
```

Capture both system and Redis metrics for 2 minutes:

```bash
docker run --rm -it --privileged \
  --network=host \
  -v $(pwd)/data:/data \
  rezolus \
  rezolus-capture --duration 2m \
    --endpoint http://localhost:9121/metrics \
    --source redis
```

The combined dashboard at http://localhost:8080 shows system-level and
Redis metrics side by side.

### System + service metrics (e.g., Valkey)

```bash
docker run -d --name valkey -p 6379:6379 valkey/valkey:latest
docker run -d --name valkey-exporter -p 9121:9121 \
  --link valkey oliver006/redis_exporter --redis.addr redis://valkey:6379

docker run --rm -it --privileged \
  --network=host \
  -v $(pwd)/data:/data \
  rezolus \
  rezolus-capture --duration 2m \
    --endpoint http://localhost:9121/metrics \
    --source valkey
```

### Just run the agent

Run the Rezolus agent indefinitely (no capture, no viewer):

```bash
docker run --rm -d --privileged \
  -p 4241:4241 \
  rezolus
```

The agent's metrics endpoint is available at http://localhost:4241.

## rezolus-capture Reference

```
rezolus-capture [OPTIONS]

REQUIRED:
  --duration <DURATION>       How long to capture (e.g., 60s, 5m, 1h)

OPTIONS:
  --endpoint <URL>            Service metrics endpoint (Prometheus-compatible)
  --source <NAME>             Source name for service metrics (required with --endpoint)
  --interval <INTERVAL>       Sampling interval (default: 1s)
  --output-dir <DIR>          Output directory for parquet files (default: /data)
  --viewer-listen <ADDR>      Viewer listen address (default: 0.0.0.0:8080)
  --no-viewer                 Skip launching the viewer after capture
  -h, --help                  Show help text
```

## Ports

| Port | Service |
|------|---------|
| 4241 | Rezolus agent (always running) |
| 8080 | Rezolus viewer (after capture completes) |

## Volumes

Mount `/data` to persist capture output on the host:

```bash
-v $(pwd)/data:/data
```

The capture script writes `capture.parquet` to this directory.

## Privileged Mode

The Rezolus agent uses eBPF for low-overhead kernel instrumentation. This
requires elevated privileges. Run the container with one of:

```bash
# Full privileged mode (simplest)
docker run --privileged ...

# Or with specific capabilities
docker run --cap-add SYS_ADMIN --cap-add BPF --cap-add PERFMON ...
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `REZOLUS_AGENT_CONFIG` | `/etc/rezolus/agent.toml` | Path to the agent config file |

## Building from Source

```bash
docker build -f docker/Dockerfile -t rezolus .
```

The Dockerfile uses a multi-stage build:
1. **Build stage**: Compiles Rezolus from source (Debian bookworm with Rust toolchain)
2. **Runtime stage**: Minimal image with just the binary and runtime dependencies
