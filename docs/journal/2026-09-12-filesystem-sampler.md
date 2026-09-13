# Filesystem occupancy sampler — local mounts only

- **Opened:** 2026-09-12
- **Status:** **SHIPPED, measured.** A `filesystem` sampler reports total, free
  and available bytes and total and free inodes per locally mounted
  filesystem (iopsystems/rezolus#1202). Measured sweep cost and the scope
  decision that made the cost bounded are in *Results* and *Decisions*.
- **Driver:** #133 asked for free-space metrics in 2023 and was closed in 2024
  as health telemetry other agents cover, "willing to revisit if there's
  sufficient demand". #1202 is that demand: a fleet that wants Rezolus as its
  only agent still had to run Telegraf's `inputs.disk` beside it for the one
  signal that pages someone — a filling disk — because `blockio` reports the
  device and `drivehealth` the drive, and nothing reported the mount.
- **Owner:** Yao Yue

## Why now, when #133 was closed on overhead-versus-value

Two things changed since #133. Per-sampler intervals exist
(`[samplers.<name>] interval`, falling back to `[defaults] interval`), so a
slow gauge no longer rides the scrape cycle; `drivehealth` (#992) established
the pattern — a throttled sweep dispatched to the blocking pool, serving the
cached value between reads — and widened the project's scope to slow-moving
health gauges when the mechanism is cheap. This sampler follows that pattern
exactly.

## Decisions

**Local mounts only, classified before any `statvfs`.** The mount table
(`/proc/self/mountinfo`) is parsed and each mount classified by type and
source (`mounts::MountEntry::is_local`): block-backed types with a `/dev`
source, plus `zfs`, are local; network types (`nfs`, `nfs4`, `cifs`, `smb3`,
`ceph`, `glusterfs`, `afs`, `9p`, `lustre`, `ocfs2`, `gfs2`, ...), every FUSE
type, autofs triggers and the kernel pseudo-filesystems are not. The rule is
fail-closed: a mount qualifies by a positive signal, never by merely missing
the deny lists.

The reason is the failure mode. `statvfs` on a network mount is an RPC; on a
`hard` mount (the default) it blocks until the server answers, and the
`timeo`/`retrans`/`soft` knobs are mount options owned by whoever mounted the
share, not by the process making the call. On a `soft` mount the worst case is
still `timeo × (retrans + 1)`, minutes. A `statvfs` on an autofs trigger
starts a mount attempt. Local filesystems answer from in-memory superblock
counters and issue no I/O. So classification is the only thing that bounds
the sweep, and it is done from a local kernel read that blocks on nothing.
`df -l` draws the same line. Network mounts stay out of scope until someone
asks; the reopen condition lives in `docs/backlog.md`, pointed at from the
sampler's module doc rather than scattered through the read path.

**A local mount that something covers is not sampled.** Found in review (Codex,
round 1 of the branch's local review thread): classifying by type alone let an
ext4 mount at `/data` through with an NFS share stacked on it, and
`statvfs("/data")` resolves to the share — no race needed, and a dead server
would have hung the inline startup sweep. `mountinfo` keeps both entries, so
the fix is structural: each mount's id and parent id are kept, and only mounts
that path lookup reaches are sampled (`mounts::visible`). Resolution starts at
the one parent id the table references but does not list — the mount holding
the process root, so a chroot table with no `/` row still resolves (round 5) —
and, at each mount point along a path, enters the mount attached there and
climbs any stack on it, as the kernel does. A first
version instead dropped a local mount when any mount at or above its path was
outside its own parent chain; round 3 showed that a *hidden* mount then
suppressed the visible one at the same path — an old `/data/sub` under a
replaced `/data` tree — which resolving from the root fixes. The read then goes
through an `O_PATH` descriptor, deliberately without `O_DIRECTORY` because
container file bind mounts such as `/etc/hosts` are local mounts too, and its
`statx` mount id must equal the table's, so numbers from a mount that replaced
the sampled one are never published. It matches on mount
id rather than `st_dev` because btrfs reports `st_dev` per subvolume. The check
follows the path lookup instead of preventing it; that residual is in
*Deferred*.

**Rescan the mount table every sweep.** `drivehealth` enumerates once at
startup. This sampler cannot: a filesystem mounted after the agent started and
then filling up is exactly the case the metric exists for. The cost is
measured below and is paid once per interval, off-cycle.

**One acquisition group for the sweep.** Five metrics, one `statvfs` per
mount, one source per entity — principle 18's device-sweep shape, as
`drivehealth`. `acquire()` before the mount-table read, `finish()` after the
last `set()`, `discard()` when nothing published (mount-table read failed, or
every `statvfs` failed).

**Stable slots, member bound follows the population.** Each filesystem, keyed
by `major:minor`, keeps its `GaugeGroup` slot while mounted; an unmounted one
has its values unset (`i64::MIN`, which the snapshot walk reads as absent) and
its labels cleared, and the slot is reused. The group's member bound is set
by the sweep task — the group's single writer — to one past the highest
occupied slot before the window is stamped, so a three-mount host is walked
as three members, not `MAX_MOUNTS` (64). This is a per-sweep write to a bound
that `timing.rs` documented as single-init when this sampler was written; it
now requires only a single writer. A membership change is an honest schema
change (a mount appeared or went away), so the V3 schema-hash churn it causes
is the truth, not noise. The store is not atomic with a snapshot: the builder
reads the bound separately for each of the five gauge families, so a sweep
landing mid-snapshot can give them different bounds. Flagged in the PR for the
maintainer's ruling.

**Dashboard.** A Filesystem section with `sum by (mount)` over each gauge, so
the viewer's multi-series chart names one line per mount and mounts that
appear or disappear show up on the next render — the same shape as the
cgroups section's `sum by (name)`. Inode cards render only when the
recording carries inode metrics; a recording without the sampler renders an
empty section.

## Go/no-go — measured

Host: x86_64 workstation, 4 real mounts in the table (3 distinct local
filesystems after bind-mount dedup: ext4 ×2, vfat ×1), kernel 7.0, unprivileged
run per the reviewing-samplers recipe (`[defaults] enabled = false`, only
`filesystem` enabled, `interval = "1s"` to force a sweep per scrape).

Pre-build estimate (Python, same host): `statvfs` 1.1–1.9 µs per local mount,
`/proc/self/mounts` read 44 µs, `/proc/self/mountinfo` read 72 µs (12 KB, 87
lines). A hot loop in the test binary (`sweep_phase_timing`, ignored test)
agrees: read 70 µs, `statvfs` ×3 3 µs, parse 200 µs in a debug build. The
agent's numbers below are higher than the sum of those parts, and the gap is
the environment, not the code: a `spawn_blocking` thread waking once per
interval runs cold — caches, clock — where the hot loop does not. The cold
number is the one production pays, so it is the one reported.

After the cover fix (see *Decisions*), the same test in a release build: read
62 µs, parse and classify including the cover check 65 µs, and 6 µs for three
`open` + `statx` + `fstatvfs` reads — about 2 µs a mount. There is no release
run of the pre-fix code to subtract, so this records the fixed cost rather
than a delta; every phase is within the cold sweep range in the table below.
After round 3 replaced the cover check with resolution from the root mount,
restricted to local candidates, the same release test read 72 µs, parsed 74 µs
and made the three reads in 7 µs. The unchanged read phase moved 10 µs between
the two runs, so the 9 µs parse difference is within run-to-run noise.

One thing tried and measured as no gain: pre-sizing and reusing the read
buffer. A fresh `String` makes `read_to_string` probe a zero-size procfs
file in doubling 32-byte chunks (13 `read()`s under `strace`); the reused
buffer reads it in one call, and the read phase did not move — the kernel
generates the table once per `open`, and that is the cost. Kept for the
allocation it saves, documented as unproven.

| build | `refresh()` on the scrape path | full sweep, off-cycle |
|---|---|---|
| debug | 3–22 µs (first call 102 µs) | 626–1139 µs |
| release | 0–8 µs (first call 91 µs) | 330–570 µs (read 220–350, parse 100–200, statvfs+publish 11–50) |

The scrape-path number is what the sample cycle pays every tick: a time
check and, once per interval, a `spawn_blocking` dispatch. The sweep number
is the mount-table read, parse, classification, three `statvfs` calls and
fifteen `set()`s, paid once per 60 s by default — an amortized cost of
10 µs per second of wall time, about 0.001 % of one core.

**Not measured:** a host with thousands of mounts. The mount table scales
with containers (a Kubernetes node can carry a few thousand overlay mounts);
the read and parse then cost low single-digit milliseconds per sweep, but the
`statvfs` count does not grow with it, since overlay and tmpfs mounts are
filtered out before any call. If that ever matters, `poll()` on the
mountinfo descriptor reports `POLLPRI` when the mount table changes, so a
sweep could rescan only when something moved — noted in the module doc, not
built.

## Results

Shipped in the PR closing #1202. Principal files: `src/agent/samplers/filesystem/`
(`linux/mod.rs` sampler and slot logic, `linux/mounts.rs` mount-table parser
and classifier, `linux/stats.rs` metrics) and
`crates/dashboard/src/dashboard/filesystem.rs`; registration in
`src/agent/samplers/mod.rs`, `crates/dashboard/src/dashboard/mod.rs` and the
analysis-side lists (`src/analysis/extract/{context,golden}.rs`); prose in
`config/agent.toml`, `docs/metrics.md`, `CHANGELOG.md`, `docs/principles.md`,
`docs/backlog.md` and the `reviewing-samplers` skill.
Tests: 21 on the parser and classifier (fixture lines for nfs, cifs,
fuse.sshfs, autofs, overlay, tmpfs, zfs, a bind-mount pair, and ten visibility
cases: a share stacked on a local mount, one mounted on a directory above it, a
local mount stacked on top, `/data` against `/database`, a covered bind alias, a
hidden tree under a replacement tree, a chroot table with no `/` row, a covered
mount in a chroot table, two omitted parents, and an ambiguous attachment), 12 on slot assignment and relabeling, `statvfs`,
its mount-id refusal and a regular-file read, vacate/label and the end-to-end
sweep against a readable local mount, 3 on the dashboard section.

## Deferred / reopen

- **Network mounts** — By design. Excluded until there is real demand
  (#1202). A future opt-in needs its own blocking budget (bounded thread,
  deadline per mount); the classification is the only thing keeping the
  sweep off a dead NFS server today.
- **Event-driven rescan** — Idea. `poll()`/`POLLPRI` on the mountinfo
  descriptor to rescan only on mount-table change. Reopen if a
  many-thousand-mount host shows the parse cost mattering at the chosen
  interval.
- **`MAX_MOUNTS` = 64** — By design. Mounts beyond the cap are dropped and
  counted in a warning per sweep. Reopen if a real host exceeds it.
- **A network mount stacked mid-sweep** — Accepted. Covered mounts are dropped
  from the table and a changed mount id refuses publication, but a network
  mount stacked over a local path between the table read and the `open` can
  still park the sweep thread on the lookup, and later sweeps skip while it
  stays parked. No unprivileged interface reaches a mount by id without a
  path. Reopen if a sweep is ever observed parked in `open`.
- **Label changes inside a `.rez` segment** — Open, #1205. A relabeled or reused
  slot keeps writing into the column created with its first labels, because the
  group table builder keys columns by descriptor name alone. `cpu_usage`'s
  per-PID task slots share the exposure, so the fix belongs in `crates/rez`
  rather than in this sampler.
