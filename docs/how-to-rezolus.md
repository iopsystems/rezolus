# How to run Rezolus in development

Practical notes for getting `rezolus agent` + `rezolus view` up on a dev
box, especially the kinds of dev box you find yourself working in
(macOS Apple Container, Docker on Linux, etc.). Captures the
not-quite-obvious bits — capability flags, scheduler policy gotchas,
how to point the viewer at a live agent vs. a parquet, and quick
verification commands.

If you're just looking for "build and test", see `CLAUDE.md`. This is
the "I actually want a running stack" doc.

## TL;DR — running locally

After `cargo build --bin rezolus`, you have two long-running pieces to
start:

```bash
# Terminal 1 — the agent (collects metrics, exposes /metrics/binary on 4241)
sudo target/debug/rezolus config/agent.toml

# Terminal 2 — the viewer (connects to the agent, serves dashboard on 8080)
REZOLUS_NO_OPEN=1 target/debug/rezolus view http://127.0.0.1:4241 \
    --listen 0.0.0.0:8080
```

Open http://localhost:8080 in a browser. Charts should populate within
~one polling interval (1 s) and continue advancing.

`REZOLUS_NO_OPEN=1` suppresses the auto-launch of a browser — useful
when running over SSH or in a container.

## The two scheduler/eBPF gotchas

Running the production `config/agent.toml` requires two privileges
that aren't always granted by default:

### 1. `CAP_SYS_NICE` for the realtime scheduler

`config/agent.toml`'s default scheduler policy is `round_robin`
(SCHED_RR). Setting that policy requires `CAP_SYS_NICE`, which **even
`sudo` inside a container won't grant** if the container runtime
didn't pre-allow it. You'll see:

```
could not set scheduler policy: 2 error: Operation not permitted (os error 1)
```

(Policy `2` is SCHED_RR.)

**Two ways to fix:**

**(a) Override the policy in the config.** Make a copy of `agent.toml`
with normal scheduling:

```toml
[general]
listen = "0.0.0.0:4241"
ttl = "10ms"

[scheduler]
policy = "normal"
niceness = 0

[log]
level = "info"
```

Save as e.g. `/tmp/agent_user.toml` and run with that path. This is
the simplest fix and what you want for development. Realtime
scheduling matters for fleet-production minimum-jitter sampling; for a
dev box, normal scheduling is fine.

**(b) Grant the capability at container startup** — see the
container-specific sections below.

### 2. `CAP_BPF` + `CAP_PERFMON` (or fallback `CAP_SYS_ADMIN`) for eBPF samplers

Most of the interesting samplers (`cpu/usage`, `blockio/{latency,requests}`,
`scheduler/runqueue`, `syscall/{counts,latency}`, `network/*`,
`tcp/*`) are eBPF-backed. Loading BPF programs needs `CAP_BPF` +
`CAP_PERFMON` (kernel ≥ 5.8) or `CAP_SYS_ADMIN` (older kernels).

If the container doesn't grant these caps, the affected samplers
fail to load at startup and you'll see warnings in the agent log
like `failed to load sampler X: Operation not permitted`. The agent
keeps running with the surviving samplers; the viewer reflects this
by graying out sections that have no data (per
`docs/troubleshooting.md` and the sidebar-graying logic in
`src/viewer/assets/lib/ui/layout.js`).

`/proc`-based samplers (memory, cgroups when present, the
agent's self-monitoring "Rezolus" section) work without any cap
beyond what root normally has.

## Container-specific setup

### Apple Container (macOS)

Apple's `container` tool runs OCI containers in lightweight Linux
VMs. It uses Docker-ish `--cap-add` flags but **does not have a
single `--privileged` switch**. You pass caps explicitly:

```bash
# Closest equivalent to --privileged — grants every Linux capability
container run --cap-add ALL --name rezolus-dev <image> ...

# Surgical — just what Rezolus actually needs
container run \
    --cap-add SYS_NICE \
    --cap-add SYS_ADMIN \
    --cap-add BPF \
    --cap-add PERFMON \
    --name rezolus-dev <image> ...
```

`SYS_NICE` alone fixes the SCHED_RR error. `BPF` + `PERFMON` enable
eBPF samplers on recent kernels (5.8+). `SYS_ADMIN` is the
sledgehammer fallback for older kernels where the split caps don't
exist.

Capabilities can't be added to a running container — you must stop
and recreate it. Commands run from the macOS host (not inside the
container):

```bash
container stop rezolus-dev
container rm   rezolus-dev
container run  --cap-add ALL --name rezolus-dev <image>
```

See https://github.com/apple/container/blob/main/docs/command-reference.md
for the full flag set. Worth knowing also exists: `--virtualization`
exposes nested-virt to the container (not needed for Rezolus, but
the option is there).

### Docker / Podman / OrbStack

Standard:

```bash
docker run --privileged --name rezolus-dev <image>
# or fine-grained:
docker run \
    --cap-add SYS_NICE \
    --cap-add SYS_ADMIN \
    --cap-add BPF \
    --cap-add PERFMON \
    --name rezolus-dev <image>
```

Same caveats — recreate to change capabilities. `--privileged` is
the path of least resistance; on production you'd want fine-grained.

### Linux host (no container)

`sudo target/debug/rezolus config/agent.toml` — root has every cap
by default. If you hit "operation not permitted" anyway, you're
probably in a user namespace (snap, flatpak, rootless container);
fall through to the `--cap-add` approach.

## Pointing the viewer at different sources

The `view` subcommand takes one positional argument that auto-detects
mode by shape:

```bash
# Live agent — argument is an HTTP URL
rezolus view http://localhost:4241

# Parquet file — argument is a path
rezolus view path/to/capture.parquet

# A/B compare — two parquet paths
rezolus view baseline.parquet experiment.parquet

# Combined-A/B tarball (auto-extracted, marked combined_ab=true)
rezolus view ab_capture.parquet.ab.tar

# Upload-only — no argument; the user uploads a parquet from the UI
rezolus view

# With a custom listen address (default is 127.0.0.1:3000)
rezolus view <input> --listen 0.0.0.0:8080
```

Common flags:

- `--listen ADDR:PORT` — where the viewer's HTTP server binds.
- `REZOLUS_NO_OPEN=1` (env var) — don't auto-launch a browser.
- `--proxy-allow <host-pattern>` — allow the in-viewer "Load URL"
  flow to fetch parquets from matching hosts.
- `--proxy-allow-any` — allow URL loads from any host (use with
  caution).
- `--category <name>` — pre-select a category template.
- `--templates <path>` — override the embedded service-extension
  templates with a directory on disk.

## "Did it actually work?" — quick verification

```bash
# Agent banner (should show "Rezolus <version> Agent")
curl -sS http://127.0.0.1:4241/

# Agent system info
curl -sS http://127.0.0.1:4241/systeminfo

# Agent's current snapshot in JSON (human-readable)
curl -sS http://127.0.0.1:4241/metrics/json | head -c 500

# Viewer mode
curl -sS http://127.0.0.1:8080/api/v1/mode

# Time range — should advance every second in live mode
curl -sS "http://127.0.0.1:8080/api/v1/metadata?capture=baseline"

# Server-side per-section status (drives the sidebar gray-out)
curl -sS "http://127.0.0.1:8080/api/v1/section_status?capture=baseline"

# A real metric query — memory_total works without eBPF
curl -sS -G \
    --data-urlencode "query=SELECT CAST(timestamp/1e9 AS DOUBLE) AS t, CAST(memory_total AS DOUBLE) AS v FROM _src WHERE memory_total IS NOT NULL ORDER BY timestamp DESC LIMIT 3" \
    --data "start=0&end=9e9&step=1&capture=baseline" \
    "http://127.0.0.1:8080/api/v1/query_range"
```

Charts that load with real data: **Memory**, **Rezolus** (the agent's
self-monitoring), **cgroups** (if cgroups v2 is mounted and visible —
typically yes inside containers). Charts that need eBPF and will
gray out without it: **CPU**, **GPU**, **Network**, **Scheduler**,
**Syscall**, **Softirq**, **BlockIO**.

## Smoke tests for changes

```bash
# Unit + integration tests
cargo test --bin rezolus
cd /work/metriken && cargo test -p metriken-query-sql

# Frontend JS tests (some legacy failures are expected — see primer.md)
node --test /work/rezolus/tests/*.mjs

# End-to-end viewer smoke (upload / file / A-B / proxy modes)
bash /work/rezolus/tests/viewer_smoke.sh

# Headless-Chromium per-section render check (file mode)
bash /work/rezolus/scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet

# Same, but against a live agent (the agent must already be running)
bash /work/rezolus/scripts/viewer_chromium_smoke.sh \
    --live http://127.0.0.1:4241 --ingest-wait 5
```

The chromium smokes write a directory under `/tmp/rezolus-chrome-*/`
with per-section screenshots, console errors, and an HTTP-failure
log. Worth opening when something goes wrong — the
`/api/v1/query_range` failures show up there even when the API itself
returns 200.

## Building variants

```bash
# Full build (default features = live-agent mode, MCP, viewer)
cargo build --bin rezolus

# Release
cargo build --release --bin rezolus

# SQL-only — drops live-agent mode and the legacy metriken-query crate
cargo build --bin rezolus --no-default-features --features sql-only

# Verify SQL-only build excludes metriken-query (should print nothing)
cargo tree -p rezolus --no-default-features --features sql-only \
    | grep 'metriken-query v'

# Developer mode — serves viewer assets from disk for hot reload
cargo build --features developer-mode
```

## Common error messages

| Message | Cause | Fix |
|---|---|---|
| `could not set scheduler policy: 2 error: Operation not permitted` | SCHED_RR needs `CAP_SYS_NICE` | Switch config to `policy = "normal"` or grant the cap at container start |
| `failed to load sampler X: Operation not permitted` | eBPF sampler needs `CAP_BPF`+`CAP_PERFMON` or `CAP_SYS_ADMIN` | Grant caps at container start, or accept the partial sampler set |
| `failed to connect to agent at http://...` (viewer) | Agent not running or wrong port | Start agent first, verify with `curl http://127.0.0.1:4241/` |
| `/api/v1/query_range` returns `capture_not_found` | Live-mode capture not attached (pre-`1d471cd` build), or upload-mode pre-upload | Rebuild — the live capture handling is on the `yv/sql-testing` branch tip |
| Charts render empty but section says "Charts with no data" | Sampler isn't running (eBPF / `CAP_PERFMON` missing) | Check agent log for sampler-load warnings |

## Cleaning up

Both processes detach with `nohup` if you started them that way.
Otherwise, plain Ctrl-C in each terminal stops them. If you've
written PID files:

```bash
kill $(cat /tmp/agent_pid) $(cat /tmp/viewer_pid) 2>/dev/null
```

To wipe any captured parquets from the viewer's tempdir:

```bash
rm -rf /tmp/rezolus-* /tmp/agent.log /tmp/viewer.log
```
