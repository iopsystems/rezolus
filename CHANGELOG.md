## [Unreleased]

### Security

- rustls-webpki 0.103.16 and webpki-root-certs 1.0.10, for RUSTSEC-2026-0382
  and RUSTSEC-2026-0383. (#1510)
- rustls 0.23.46, for RUSTSEC-2026-0346 through 0349. (#1467)
- No `thrift` (GHSA-2f9f-gq7v-9h6m), through parquet 60, and no `lru` 0.12
  (GHSA-rhfx-m35p-ff5j), through metriken-query 0.37, metriken-storage 0.3
  and ratatui 0.30.

### Added

- **Ubuntu 26.04 (resolute) packages.** amd64 and arm64 packages are built
  for resolute and published to the `ubuntu-resolute` apt repository;
  `install.sh` installs from it. 5.x ships them from 5.29.0 (#1573). (#1577)
- **Network traffic on macOS.** `network_traffic` now runs on macOS,
  unprivileged, and sets `network_bytes` and `network_packets` (every
  interface) and `network_host_bytes` and `network_host_packets` (the
  interfaces backed by hardware, such as wifi, Ethernet and Thunderbolt
  networking, plus `awdl0`, `llw0` and `ap1` on the wifi radio; not
  loopback, bridges, tunnels or VM interfaces), so the network dashboard's
  traffic charts and the overview's network chart draw on a Mac.
- **TCP counters on macOS.** `tcp_mib`, `tcp_retransmit` and `tcp_traffic`
  now run on macOS, from the `net.inet.tcp.stats` sysctl, and set
  `tcp_retransmit`, `tcp_segments_retransmitted`, `tcp_delayed_acks`,
  `tcp_rto_expirations`, `tcp_checksum_errors`, `tcp_input_errors` and
  `tcp_bytes`. Received bytes leave out segments that arrive at the next
  expected byte while later data is queued for reassembly, and count
  out-of-order overlaps twice. The kernel's counters are 32 bits; while
  `tcp_traffic` runs the agent reads them every 250 ms, which keeps its
  64-bit totals exact below 128 Gbit/s. While
  `net.inet.tcp.disable_access_to_stats` is set (the default), the kernel
  returns zeros to a process not running as root, so the agent needs root;
  without it the samplers report themselves unsupported.
- **Uninterruptible sleep (D state) from the `scheduler` sampler.**
  `scheduler_uninterruptible` is the distribution of off-CPU intervals spent
  in uninterruptible sleep (disk IO, some locks, page faults on slow
  storage), a subset of `scheduler_offcpu`.
  `scheduler_uninterruptible_wait` (per CPU) and
  `cgroup_scheduler_uninterruptible_wait` (per cgroup) count the
  nanoseconds. Each interval is added in full when the task next runs, so
  over windows much longer than the sleeps their rate is the mean number of tasks
  in D state, and a long sleep shows as a spike when it ends. Idle kernel
  threads (`TASK_IDLE`) are left out, as the load average leaves them out.
  The scheduler and cgroups dashboards plot them. (#1589)

### Fixed

- **A histogram no sampler has read is no longer published.** The agent
  leaves a histogram out of its snapshots and stream until its acquisition
  group is first stamped. A macOS agent published the Linux BPF histograms
  (`tcp_packet_latency`, `scheduler_runqueue_latency`, `syscall_latency`,
  the ext4 and xfs latencies, and others), the `blockio` latency and size
  histograms and `tcp_size` with no values, so every macOS recording carried
  them and the dashboards' presence checks drew their charts empty. On Linux
  a histogram now appears from its sampler's first refresh, and a disabled
  sampler's histograms are absent.
- **`memory_pagecache` loads on 6.18 and later kernels.** From 6.18
  `folio->flags` is a `memdesc_flags_t` struct, so the CO-RE relocation for
  the integer read of it failed, the verifier rejected the fill and eviction
  programs, and the sampler failed with `Invalid argument (os error 22)`. The
  flags word is now read from `flags.f` where the kernel wraps it. (#1590)
- **`scheduler` reads the outgoing task's state from `sched_switch`'s
  `prev_state` argument where the kernel has one** (5.18 and later, with
  BTF). Read from the task, the state could already be `TASK_WAKING` from a
  concurrent wakeup, and an uninterruptible sleep in that state lost its
  D-state flag. Where the kernel's BTF shows no such argument (mainline
  before 5.18, or no BTF) the state is still read from the task, so there
  `scheduler_uninterruptible` and the `*_uninterruptible_wait` counters can
  undercount by an amount that depends on the workload: the more sleeps end
  with a wakeup during the switch that started them, the larger it is.
  (#1596)
- **`memory_writeback` loads on kernels whose `enum wb_reason` has no
  `WB_REASON_LAPTOP_TIMER`, such as 7.0.** The CO-RE relocation
  for it failed, the verifier rejected `writeback_start`, and the sampler
  failed with `Invalid argument (os error 22)`. Each reason is now compared
  only when the running kernel's enum has it; `writeback_runs` with
  `reason=laptop_timer` stays at zero on kernels without it. (#1572)
- **The macOS agent no longer leaks memory on every CPU sample.** The
  `cpu_usage` sampler never freed the array `host_processor_info` returns,
  so each refresh leaked one page (16 KiB on Apple Silicon), about 1.4 GB a
  day at one scrape a second.
- **A single selected cgroup keeps its colour after a zoom or refresh.** With
  one cgroup selected, a zoom drew its individual charts as "Series 1" in a
  colour other than the picker's swatch, and a live refresh drew them as a
  plain line in the accent colour. The display path now names a lone series
  on a `multi` chart, as the JSON path does, and the individual cgroup charts
  are `multi` in the dashboard definition rather than only on the plots
  present when the selector mounted. (#1289)
- **Hardware Sensors series have short names.** Each chart was grouped by
  `sensor`, the full sysfs identity (for example
  `hwmon:/devices/pci0000:00/0000:00:02.1/0000:02:00.0/0000:03:08.0/0000:0b:00.1:bnxt_en:temp1`).
  `hw_sensors` now also labels each channel with `device`, the last component
  of the device path, and `name`, `<chip> <label or channel> <device>`
  (for example `bnxt_en temp1 0000:0b:00.1`), and the charts group by
  `name`. A recording without `name` (from an older agent) is still grouped
  by `sensor`; one that mixes both sums its series without `name` into one
  line. (#1518)
- **NVMe health metrics carry an acquisition window on Linux.** The Linux
  `drivehealth` sweep never acquired its NVMe acquisition group, so the
  temperature-time and thermal-management counters and the
  `drive_temperature_sensor` gauges published with no window. The sweep now
  stamps it when any drive's health log read, as the macOS sampler does. A
  recording now holds one NVMe row per sweep instead of one per tick, and
  `rate()` on these counters gets an uncertainty band. (#1548)
- **A per-entity line chart has no legend.** A chart with one line per
  sensor, mount or cgroup drew a legend that wrapped to as many rows as it
  had series, over the plot and the time axis. The tooltip names every series
  at the cursor, and the plot now starts where a single-series chart's does.
  Clicking a legend entry hid that series; per-entity charts lose that. The
  compare-mode overlay keeps its two-entry legend. (#1517)
- **A compare chart's decimated baseline draws an unbroken line, and a
  decimated envelope breaks at a missing bucket.** A compare chart's baseline
  arrives in decimated buckets while its experiment arrives at the
  recording's interval, and both were gapped against the interval, so the
  baseline drew as dashes. Each series is now gapped and stepped by its own
  spacing: one spaced at least 1.5 times the interval draws unstepped, which
  also turns a slow sampler's series on a line chart (for example 5 s sensor
  readings in a 1 s recording) from isolated points into a line. A decimated
  per-entity chart's min/max envelope and uncertainty ribbon spanned a
  missing bucket that its line broke at; they now break there too. (#1519)
- **A cgroup whose name holds a backslash or a quote can be selected.** The
  picker put selected names into the query unescaped, so systemd's `\x2d`
  (an escaped `-`) was consumed by the PromQL string and the cgroup was
  neither shown on the individual side nor removed from the aggregate. Names
  are now quoted for the PromQL string. A cgroup is offered for selection
  when its name is letters, digits, `/ _ . : @ - = , ~ %` and `\xHH`
  escapes, which cover systemd units, container ids and pod names; any other
  name is shown disabled with the reason, and a link that selects one drops
  it with a console warning. (#1523)
- **`task_cpu_usage` drops a series within one snapshot interval of its thread
  exiting, even when the exit event is lost.** A `task_exit` dropped because
  its ringbuf was full left the thread's labels exported at 0 until the pid
  was reused; measured earlier, 1,573 such series under heavy thread churn
  more than five minutes after their threads exited. After each drain the
  sampler now checks every labelled task slot against the BPF side's
  `task_start_times` and releases the ones whose instance has exited or been
  replaced, and every slot of a thread group whose last thread has exited
  (when that exit's event is delivered). This also covers a rename that races
  its thread's exit (#1514), and the old thread id of a thread whose exec
  failed after taking the leader's pid.
- **The cgroups section's aggregate charts leave out the selected cgroups.**
  The aggregate charts are the sum over the cgroups not selected, but the
  viewer removed their `name!~` exclusion before every query, so a cgroup
  moved to the individual side was still counted on the left. The exclusion is now filled with the selection and removed only when
  nothing is selected. (#1289)
- **A per-entity heatmap redraws in one frame.** Above 3000 cells the
  heatmap was drawn progressively: each redraw cleared the series and
  repainted it in 8000-cell chunks over several frames, which a live view
  showed as a flash on every refresh. The threshold is now 60000 cells, and
  the heatmap already merges columns to stay under about 50000, so an
  unzoomed heatmap draws in one frame; a zoom to a finer resolution can still
  exceed the threshold and draw progressively. 57600 cells took 169 ms in one
  frame against 270 ms over 6 frames progressively, in headless Chrome.
  Bucket and quantile heatmaps are unchanged. (#1525)
- **A decimated multi-series line chart draws continuous lines.** Once a
  range holds more samples than the chart's point budget, the viewer fetches
  one point per bucket, and the bucket is wider than the recording interval.
  The gap check compared consecutive points with the interval, so each point
  was drawn as a short dash followed by a gap, percentile uncertainty bands
  were not drawn at all, and the min/max envelope behind the lines joined the
  dashes with slanted fills. The gap check now uses the spacing between
  buckets, and decimated lines are drawn unstepped like the single-series
  decimated median. Charts at native resolution are unchanged. The envelope
  still spans a bucket that is actually missing. (#1516)
- **A process that execs from a thread other than its leader keeps its
  `task_cpu_usage` series.** `task_info` and `task_exit` travel on separate
  ringbufs, and each drain applied every `task_info` first. The old leader's
  exit therefore removed the labels the exec'd thread had just been given
  for the same pid, and the process had no per-task series until it was
  renamed or the pid was reused. Both events now carry a timestamp, and an
  exit older than the slot's last labels leaves them in place. (#1512)
- **`cpu_usage` and `cgroup_cpu_usage` no longer count an exiting task's CPU
  twice.** `sched_process_exit` fires partway through `do_exit`, and the
  sampler cleared the task's slot there. A tick in the rest of `do_exit`
  (closing files, task work, `exit_notify`, and from Linux 6.16 `exit_mm`)
  found the slot empty, took the task for a new one and, when it had started
  after the agent, counted its whole lifetime CPU again. Measured on Linux
  6.12, a process that burned 2 s and exited holding 200k open files was
  counted at 1.99 times the cgroup's `cpu.stat`. A thread that execs from
  outside the group leader, which takes the leader's pid and start time, had
  its CPU before the exec counted twice the same way (1.66 times `cpu.stat`
  for a thread that burned 2 s, exec'd and burned 1 s), and its own thread id
  kept its labels and a frozen value until the pid was reused. The exit now
  marks the slot exited and keeps the baselines; `sched_process_exec` starts
  the exec'd thread under the leader's pid from its own baselines and retires
  its old thread id. (#1508)
- **A multi-endpoint `.dendro` recording no longer stops when two sources
  record a group of the same name, one long and one wide.** The writer kept
  one set of long stream names for every source, so the wide source's rows
  were sealed as long, the seal failed with `failed to decode a WAL long
  row: array had incorrect length`, and that source stopped recording. A
  copy of such an archive (a hindsight dump, `recording snapshot`, filter or
  combine, Save-as-Report) failed the same way on its unsealed rows. Fixed
  in metriken-storage 0.4.
- **`task_cpu_usage` follows thread renames.** `comm` was read once, when
  the sampler first accounted the thread, so a thread that renamed itself
  afterwards (`pthread_setname_np`, `prctl(PR_SET_NAME)`, exec) kept its
  creator's or its previous name for the rest of its life. The sampler now
  attaches to the `task:task_rename` tracepoint when `task_attribution` is
  on and relabels the thread's series; a rename starts a new series. (#1489)
- **The viewer's Metadata section reads a combined recording's templates
  where 5.x `recording combine` wrote them**,
  `per_source_metadata.<source>.<entry>.service_queries` (a 5.x combined
  parquet, upgraded). It looked one level up and listed no templates.
- **The viewer escapes recording text it puts into chart HTML.** Chart
  tooltips, the heatmap tooltips and the embed demo's status line inserted
  strings from the recording as markup: plot titles from section templates
  (which an agent can send in its stream handshake), KPI titles and
  `unit_system` values from `service_queries`, series label values, heatmap
  row labels, capture labels and template format labels. Markup in any of
  them was parsed by the browser, and could run script, in both the server
  and the static viewer. They now go through one HTML-escape helper
  (`src/viewer/assets/lib/html.js`), which the Markdown renderer and, as a
  precaution, the band-swatch row (whose labels are constants) also use.
- **An unknown `unit_system` is refused.** The viewer printed an unknown
  unit system's name as the unit, so a section template with one fails to parse,
  `recording annotate` refuses KPIs with one (exit 1, nothing written), and a
  KPI template directory with one fails to load. A recording that already
  carries one renders that KPI with the default `count` scaling, and the
  viewer warns once when it loads the KPIs. The known systems are `time`, `bytes`, `bitrate`, `datarate`,
  `percentage`, `frequency`, `power`, `count` and `rate`.
- **Notebook and report Markdown links resolve the URL before allowing
  it.** A link is kept only when it resolves to `http:`, `https:` or
  `mailto:` (relative paths and anchors included). The previous check read
  the raw text, so a scheme the browser's URL parser would still find, such
  as one behind leading control characters, was allowed through.
- **metriken-storage 0.3.1 and metriken-exposition 0.23.2.** A table queried
  while a writer sealed it no longer loses the sealed rows, a reused segment
  number no longer returns rows twice, and a table or occupant's labels sealed
  while an archive opens are no longer dropped (the viewer, `mcp` and
  `recording` reading a live hindsight buffer or recording).
- **A Prometheus label value containing `=`, `,` or `}` is recorded intact**
  (metriken-recorder 0.3.1). `record` from a Prometheus
  endpoint stored `op="a=b"` with no `op` label, `op="a,b"` as `op="a"` and
  `op="x=1,y=2"` as `y="2"`, dropped a series whose value held `}`, and kept
  `\"`, `\\` and `\n` escaped. The parser was the `prometheus-parse` crate,
  used by every output format, so 5.x `.rez`, parquet and raw recordings lost
  the same labels; reading an existing recording does not change. Pinned by
  `a_prometheus_label_value_is_stored_as_the_exposition_spells_it`. A line
  the exposition parser cannot read is skipped and logged once per endpoint.

### Removed

- **Breaking: every command but `rezolus recording upgrade` opens
  `.dendro` archives only.** A `.rez` archive (v1, v2 or v3), a standalone
  `.parquet` file and a `.parquet.ab.tar` A/B tarball are refused by
  `view` (a file, a browser upload, a URL load and an attached
  experiment), the static-site viewer, `mcp` (CLI and stdio), Save as
  Report and every other `recording` subcommand, with the command that
  converts the file: `rezolus recording upgrade FILE`, which writes
  `FILE`'s name with a `.dendro` extension. Nothing converts a recording
  when it is opened, in memory or into a temporary file. `upgrade` now
  also converts a `.parquet.ab.tar` into a two-recording `.dendro`
  labelled `arm=baseline`/`arm=experiment`, which the viewer opens as the
  comparison; the tarball's aliases and category are not carried over. The
  edit commands' `-o` no longer converts a legacy input; it still writes
  the edited copy of a `.dendro` elsewhere. `recording filter` loses
  `--queries` and `--templates` (they chose a parquet file's KPI columns),
  and `recording metadata` loses `--schema`, `--file`, `--geometry` and
  `--field` (they described a parquet footer). A two-file comparison
  (`rezolus view a.dendro b.dendro`, the browser's experiment upload, a
  URL pair) takes a one-recording `.dendro` as the experiment. On the
  command line the baseline is one recording too: `rezolus view a b` with
  an archive of several recordings as `a` exits with an error (a second
  file beside a `.rez` or `.dendro` used to be ignored with a warning). In
  the browser and through the upload API the baseline may also be an
  archive that opens as a comparison, whose experiment the second file
  replaces; Save as Report is then refused, since a report copies whole
  files and would hold every recording of the archive. Beside an archive
  that opens as one combined view, an experiment is refused. A
  one-recording baseline loaded in place of another keeps an experiment
  that is a file of its own. Each refusal of a file names the `recording
  combine` and `view --baseline/--experiment` commands that compare two
  recordings. The static-site demo recordings are `.dendro` files; a link
  that names a demo's old `.parquet` file loads its `.dendro`.
- **The viewer's Time selector (Aligned/Raw) is gone.** Every dashboard
  chart is on the step grid. Raw mode is a per-chart view: the expanded
  chart (the open-in-new-tab link) has SHOW SAMPLES, which reruns its query
  with a point at each sample's own timestamp, and a link's `time=raw`
  opens an expanded chart that way; the MCP `viewer_link` tool requires
  `chart_id` with `time: "raw"`. On a dashboard, Raw mode drew a query that
  combines series with each series held at its latest sample, so a ratio of
  two samplers looked as exact as one series; on the grid every series is
  read at the same instants.
- **`record` writes `.dendro` only.** The output must end in `.dendro`
  (`rezolus.dendro` without `-o`). `-o` ending in `.rez`, `.parquet`, `.raw`
  or anything else, `--format` other than `dendro`, and `--separate` are
  refused as usage errors (exit 2) before any endpoint is contacted, as are
  the same `output`, `format` and `separate = true` in a `--config` file.
  The refusal quotes the path as it was given (`-o`, the positional OUTPUT,
  or the config file's `output`) and names a 5.x recorder (5.27.0 or later),
  which still writes those formats; `rezolus recording upgrade` converts a
  `.rez` or a parquet file.
- **The `recording` tools write `.dendro` only.** metriken-storage 0.5 has
  no `.rez` writer.
  - `combine` writes one `.dendro` from `.dendro` inputs, a recording per
    input recording; `--ab` is kept and labels each input's recordings
    `arm=baseline`/`arm=experiment`. An output ending in `.parquet` (the
    row-merge), `.parquet.ab.tar` or `.rez` is refused, naming `-o
    <name>.dendro`, and so are `--pinned`, `--category` and
    `--bypass-time-check`, which applied to those outputs.
  - `filter`, `annotate` and `check --annotate` edit a `.dendro` in place,
    or a copy given `-o`. `annotate`'s `--node`, `--source` and `--filter`
    set a parquet footer key or dropped parquet columns; they are refused
    (exit 1), naming what to use instead.
  - `upgrade` converts any `.rez` (v1, v2, v3), `.parquet` or
    `.parquet.ab.tar` into a `.dendro` (without `-o`, the input's name with
    `.dendro` for its extension, a `.parquet.ab.tar` losing both; `upgrade
    x.dendro` says it is already one); `--to v3` is refused.
  - `convert` turns a raw msgpack recording into a `.dendro`.
  - The MCP server's `add_event`, `remove_events` and `run_checks`
    (`annotate=true`) take an `output` path, to write the edited copy there
    as `-o` does. `export_query` is unchanged.
- **Save as Report writes a `.dendro`**, in the server viewer and the static
  site. A two-file comparison is saved with `arm=baseline`/`arm=experiment`,
  so it reopens as the comparison.
- **`record` refuses every Rezolus agent that cannot stream.** An agent older
  than 5.21.0, a v2 agent, or a stream route that answers 404, 409 or a
  foreign content type is refused at startup (exit 1, nothing written) or,
  if it comes up later or fails on a reconnect, left out of the recording
  while the rest is recorded (exit 1). The refusal points at a 5.x recorder,
  which scrapes such an agent. A 5xx or a handshake timeout is still retried
  each tick. Before, this applied to `.dendro` output only, and `.rez` and
  parquet output scraped the agent.
- **Hindsight writes `.dendro` only.** An `output` not ending in `.dendro` is
  refused when the config is loaded (exit 1), as `record` refuses one. 5.27.0
  wrote every such path as a `.rez` archive (`.parquet` with a warning); the
  6.0 alphas kept a `.rez` buffer for `.rez` and wrote a `.dendro` for any
  other path.

  Upgrade note: 5.27.0 shipped `/etc/rezolus/hindsight.toml` with
  `output = "/var/lib/rezolus/rezolus.rez"`. A package upgrade replaces that
  file only if it was never edited (dpkg conffile, rpm `%config(noreplace)`);
  for an edited one dpkg asks which to keep (and keeps it under
  `--force-confold`), and rpm keeps it and writes the new one beside it as
  `hindsight.toml.rpmnew`. If the kept file still names a `.rez`
  (or any path not ending in `.dendro`), `rezolus-hindsight` exits 1 at start
  until `output` is changed to a `.dendro` path.
- **`record` no longer fetches an agent's `/samplers`.** The sampler status
  was written into parquet output only.

- **The live viewer no longer reads `/metrics/binary`.** `rezolus view
  http://agent:4241` needs `/metrics/stream`: an agent from 5.21.0 with
  `snapshot_format = "v3"` (the default). An older agent, a v2 agent, a
  `rezolus exporter`, or a proxy that does not route `/metrics/stream` is
  refused with its version. Save capture in live mode returns a `.dendro`
  archive, not parquet.
- **Hindsight no longer reads `/metrics/binary`.** It needs `/metrics/stream`:
  an agent from 5.21.0 with `snapshot_format = "v3"` (the default). An older
  agent, a v2 agent, a `rezolus exporter`, or a proxy that does not route
  `/metrics/stream` is refused at startup.
- **Ubuntu 20.04 (focal) packages are no longer built.** Focal ships clang 10,
  and the `memory_writeback` sampler's CO-RE enum relocations
  (`bpf_core_enum_value`) need clang 12, so the focal package no longer
  compiles. Focal's standard support ended in May 2025 and its GA kernel
  (5.4) is below the 5.8 minimum. Its repository keeps serving 5.22.1, the
  last focal release; `install.sh` still installs it there, with a warning.

### Changed

- **The memory dashboard draws the Buffers, Swap Cached, Committed and Commit
  Limit charts and the NUMA group only when the recording has them.** A
  macOS recording has none of them, and a Linux kernel without `CONFIG_NUMA`
  has no NUMA counters.
- **The network dashboard draws the Packet Drops, TCP Retransmits and TCP
  Packet Latency charts only when the recording has them.** A macOS
  recording has no `network_drop` or `tcp_packet_latency`, and has
  `tcp_retransmit` only when the agent could read the kernel's TCP
  statistics (as root, by default). The Errors and TCP
  groups are left out when none of their charts are drawn.
- **`cpu_usage` on macOS is per CPU.** It was three host-wide counters; it
  is now one series per CPU in each of `user` and `system`, labelled `id`,
  so the per-CPU charts draw on a Mac. Niced time is counted
  as `user`, as on Linux, and the `nice` state is gone.
- **`hw_sensors` is on by default.** It follows `[defaults]` like other
  samplers. On five lab hosts at the 5 second default it cost at most 0.08% of
  a core. Sweeps took 22 to 27 ms median on two x86 hosts, 323 ms on a board
  whose embedded-controller driver (`asusec`) waits about 300 ms a refresh,
  and under 1 ms on a Raspberry Pi; reads run on a blocking worker, not the
  scrape path. It no
  longer reads ACPI `Processor` cooling devices, one per logical CPU, or
  `nvme` or `drivetemp` hwmon chips, whose reads send the drive a
  command; `drivehealth` reads each drive's temperature at its own interval
  (it needs root), and NVMe per-sensor temperatures are no longer collected.
  An `asusec` temperature channel reading -40 °C, the driver's value for an
  empty header, is skipped. `[samplers.hw_sensors] enabled = false` turns it off, and an
  existing config that sets it keeps the sampler off.
- **metriken 0.11.4, metriken-exposition 0.25, metriken-model 0.2,
  metriken-storage 0.5.4, metriken-query 0.42.2, metriken-recorder 0.5.1.** The
  counting histogram types are named for their kind (`AtomicCounterHistogram`,
  `RwLockCounterHistogram`, `CounterHistogramGroup`).
- **The sample-timestamp (jitter) plot works for a `.dendro` recording**
  (metriken-query 0.42.2, metriken-storage 0.5.4); it was empty for every
  `.dendro`. It plots the instants at which the recording's first source
  with rows was read, in any sampler; a second recorder of the same agent
  is not merged in. The first request after the viewer opens or refreshes
  a recording reads every table's timestamps (the read took 6.6 s on a
  9.6 h, 49-table recording); later requests reuse them. The
  timestamp read does not add the segments it opens to the segment cache
  (metriken-storage 0.5.4), so it does not evict segments that chart
  queries cached.
- **A recording and its copies are read once.** metriken-query 0.42 forms
  process groups: sources that share a uuid (a source and its snapshot or
  hindsight dump), or share a `producer_epoch` and overlap in time (one agent
  process recorded twice), are one recording; one epoch with disjoint spans
  is two. `rezolus view`, `mcp` (its `--recording` selectors and
  `describe-recording`), `recording check` and `annotate` list and select
  these recordings, and an edit of one is written to each of its sources.
  `recording combine` no longer refuses inputs that share a source uuid. A
  query's `__recording__` is the archive index of a recording's first
  source.
- **Archives written are dendro schema 5** (dendro 0.6, metriken-storage
  0.4, metriken-query 0.41, metriken-exposition 0.24, metriken-recorder
  0.4). Every `.dendro` this release creates (`record`, hindsight buffers
  and dumps, the live viewer's save, `recording` combine, filter, snapshot
  and `upgrade --to dendro`, and the viewers' Save-as-Report) has dendro's
  `documents` and `segment_indexes` tables. Rezolus 5.28.0 and earlier, and
  6.0 builds before this change, refuse such a file with `unsupported
  archive schema version 5`. Schema 4 archives still open and read the same;
  `recording annotate` edits one in place and leaves it at schema 4.
- **An endpoint's `role=` outranks `--metadata role=`.** `record
  --endpoint url,role=service --metadata role=x` now records `role=service`,
  where the run-wide `--metadata` value won before. A per-endpoint key
  (`role=`, `template=`) beats the same key from `--metadata`, which applies
  to every recording.
- **An archive of several recordings of one run opens as one combined
  view.** `rezolus view run.dendro` on `record --endpoint a --endpoint b`'s
  archive, and the static viewer given it, show one capture: every series
  carries its recording's labels (`source`, `host`, `--label`s), and each
  template renders once per recording that carries it. Two agents show the
  built-in sections once per host, the second host's at routes ending in
  `@<host>` (`#/cpu@web-02`), grouped by host in the sidebar; an agent and a
  service show the agent's sections next to the service's dashboard from the
  template in its recording. Every query of a recording's sections is
  restricted to that recording by its labels (`{host="web-02"}`, or
  `{role!="x"}` for a recording that lacks a label another carries), the
  section's header (interval, source, version, time range) is the
  recording's, and its charts are chosen from its own metrics.
- **A/B is opt-in: an `arm` label, or `--baseline`/`--experiment`.** An
  archive opens as a comparison only when its recordings carry `arm` or the
  viewer is asked for one; nothing is inferred from when the recordings were
  taken, since an A/B can be recorded in one run. There are three ways to say
  it: `rezolus record --endpoint http://a:4241,arm=baseline --endpoint
  http://b:4241,arm=experiment` (new: `arm=` labels that endpoint's
  recording, over a run-wide `--label arm=`); `rezolus recording combine
  a.dendro b.dendro --ab baseline=a.dendro experiment=b.dendro -o ab.dendro`
  (new: each value names an input by its path or file name, every recording
  of it is labelled `arm=baseline` or `arm=experiment`, and an input
  recording that already carries another arm is refused before anything is
  copied, naming the input; `--ab` never replaces an arm); and `rezolus view
  ab.dendro --baseline host=web-01 --experiment host=web-02`
  (`WasmCaptureRegistry::set_compare_requested`, or `?compare=1` on the static
  site). The arm values fill the slots, whatever the archive's order:
  `arm=baseline` is the baseline and `arm=experiment` the experiment
  (matched ignoring case, so `Baseline` counts); with only one of the two,
  the other arm fills the other slot; with neither, the archive's first
  armed recording is the baseline and the viewer says so. Recordings
  without an arm, and with `--baseline`/`--experiment` every recording they
  do not name, are shown beside the comparison: each in its own sections
  (routes ending in `@<key>`: its host, or the labels that tell it apart;
  grouped under its name in the sidebar), drawn as a single capture that
  reads only that recording, charts pinned from them included, and named
  in the badge after the arms ("also: …"), never as a third arm. Their
  system info and metadata are listed with the baseline's, and a
  comparison whose baseline has no system metrics opens on its first
  service section. A beside recording's charts are drawn over the
  comparison's time range, not its own: an agent recorded longer than the
  arms shows only the rows inside the arms' span. So `record
  --endpoint svcA,arm=baseline --endpoint svcB,arm=experiment --endpoint
  agent` compares the two services and shows the agent's sections beside
  them. One recording per arm is the limit for now: one arm among unarmed
  recordings, an arm two recordings share, or more than two arms is
  refused with a listing of the recordings and `--baseline`/`--experiment`
  to name two. `record` and a
  config file refuse an `arm` value with whitespace around it; `recording
  combine --ab` refuses a result whose arms would be refused, and warns
  without `--ab` when the inputs' own arms would be. **An archive assembled by
  `recording combine` before this release
  carries no `arm` and now opens combined.** When its recordings' labels
  differ, `rezolus view ab.dendro --baseline source=redis --experiment
  source=valkey` (any labels that name the two; `rezolus mcp
  describe-recording ab.dendro` lists them) opens it A/B as before. When they
  are identical (two agents recorded as `source=rezolus` on one host), no
  selector can name either one; label them by position instead: `rezolus
  recording combine ab.dendro --ab baseline=0 experiment=1 -o ab-arms.dendro`
  (the `(position N)` `describe-recording` prints for each recording), which
  copies the archive with the two arms and opens A/B. `combine` takes one
  input only in this form.
- **A combined archive that is still being written is followed as one
  capture,** recordings with identical
  label sets included; a recording whose rows have not arrived at the open
  has its sections from the start, and one added while followed gets them
  when found, with a warning to reload the page. Each host's Overview shows
  the throughput of a service recorded on that host (none for a host with
  no service), and System Info lists each recording's hardware under its
  host.
- **A single-source parquet's `source` labels its upgraded recording.**
  `rezolus recording upgrade run.parquet` labels the recording with the
  file's `source`, so a KPI template carried in the parquet (llm-perf's)
  whose queries select `{source="llm-perf"}` matches its series. A combined
  parquet, whose columns carry their own `source`, and a parquet any of
  whose series already carry a `source` label are converted unlabelled.
- **`rezolus mcp` reads an archive of one run combined.** Without
  `--recording` (or the stdio server's `recording` object), `query`,
  `describe-metrics`, `detect-anomalies` and `analyze-correlation` read every
  recording together, each series under its recording's labels, and exit 0
  with the merged result, where before they exited 1 with a listing of the
  recordings; `describe-recording` lists the recordings and says so. A query
  written for one host can now match nothing: `sum(irate(cpu_usage[5m])) /
  cpu_cores` over two hosts divides one series by two (`cpu_cores` has one
  per host) and answers "No series matched". Use `sum by (host)
  (irate(cpu_usage[5m])) / on (host) cpu_cores`, a matcher
  (`cpu_cores{host="web-01"}`), or `--recording host=web-01`.
  `extract-features`, whose record describes one recording, and every tool
  on a comparison (recordings carrying `arm`) still
  need a selector. `recording check` is unchanged: it evaluates each
  recording's own KPIs against that recording.

- **The `/metrics/stream` interval is negotiated** (metriken-exposition
  0.23.1, metriken-recorder 0.3). The agent serves a stream at the larger of
  the interval asked for and its snapshot TTL, and names the interval served
  and the TTL in the response headers and the stream's handshake, where it
  served any interval asked for and sent mostly empty frames below the TTL.
  `record`, hindsight and the live viewer take the interval a producer serves
  as the source's interval: its `sampling_interval_ms`, the silence after
  which a stream is reconnected, and the wait for a frame after a wrapped
  command exits. A reconnect asks for at least the producer's floor.
  `/status` counts a subscriber at the interval it is served. At the default
  10 ms TTL an interval of 10 ms or more is served as asked; the handshake
  gains the two keys either way, and a recorder on metriken-recorder older
  than 0.3 (6.0.0 alphas before this) keeps them in a recording's metadata.
- **The agent reuses a snapshot for three quarters of its TTL**, counted from
  the moment it was asked for, where it reused it for the whole TTL counted
  from the start of its pass. A subscriber served at an interval equal to
  the TTL wakes about one TTL after its last snapshot, a little early or
  late, and an early wake was handed that snapshot again and sent an empty
  frame: an agent with a 1 s TTL, recorded at 100 ms and so served at 1 s,
  delivered 19 to 21 rows in 30 s. The agent now samples at most once per
  three quarters of its TTL.

- **Both viewers take a source's KPI template from the recording, ahead of
  the built-in one.** The `service_queries` key of a dendro source's
  metadata (what `rezolus recording annotate` writes) renders by default;
  the built-in registry is consulted for a source only when its metadata
  has none. `rezolus view
  --registry-templates` renders the built-in template instead for sources
  the registry knows. The WASM viewer exposes the same switch as
  `WasmCaptureRegistry::set_prefer_registry_templates`, which the static
  site has no control for yet. The server viewer read `service_queries`
  only from a parquet's footer, so an archive showed no KPI sections, and
  the static-site viewer matched the registry by source name only, so a
  template embedded by `annotate` showed on `rezolus view` and not in the
  browser. Each recording of a multi-recording archive uses its own
  template; when two recordings have the same source name, the baseline's
  renders for both. A template that does not parse is replaced by another
  template the metadata carries for the source, else the built-in one, else
  no KPI sections, with a warning once per distinct message (the server's
  log, the browser console), not a failed load. In a comparison, a notice
  describes that recording's own lookup; the baseline's template renders for
  a source name both recordings share. A top-level template on a file whose
  `source` names several sources (an upgraded combined parquet's JSON
  array) is keyed by the template's own `service_name`; one that does not
  parse there is ignored. A live agent's handshake metadata is read only by the terminal UI
  (`--tui`), which rebuilds its sections each tick; the browser dashboard of
  a live agent builds its sections once, with no KPI sections. The server
  viewer now also classifies a dendro source from its metadata, so a simple
  capture's section is named by its `source` (`/source/127.0.0.1-18651`)
  rather than the file name, as the WASM viewer names it. The `dashboard` key holds the
  section template for the built-in sections (see Added).

- **`memory_slabinfo` publishes `memory_slab_cache_bytes` for every slab
  cache**, labeled by `cache`, so slab growth that `memory_slab` shows is
  attributable to a cache by name (`kmalloc-*`, ZFS, KVM, VFIO or any other).
  Caches that appear or go between sweeps, as modules load and unload, take
  and release series, and a cache that is no longer listed reads absent in
  bytes and object counts alike (both used to keep their last value). At
  most 1024 caches are published. `caches = "fixed"` in
  `[samplers.memory_slabinfo]` keeps bytes to the previous eight caches;
  `caches` anywhere else, `[defaults]` included, is refused at startup.
  `memory_slab_cache_objects` still covers the eight filesystem metadata
  caches only. The bytes are an upper bound per cache: the kernel reports
  pages per slab at the preferred order, and slabs allocated at the fallback
  order under memory pressure hold fewer. The Memory dashboard draws the eight
  metadata caches one line each and sums the rest into one line.

- **Both `memory_slab_cache_*` metrics changed label sets.** They are now
  slot-keyed groups, so each series carries the slot `id` label beside `cache`
  (and `state`), and recordings carry the internal `__uid__`, which the
  Prometheus exporter strips. A Prometheus server sees a new series for every
  cache across the upgrade, and a cache whose module unloads and loads again
  can come back on a different `id`.

- **`rezolus view` renders a dashboard section once its counter and gauge
  charts have data**, and its histogram charts show as loading until theirs
  land; the histogram queries are sent after the others have answered,
  where every query was sent at once and the browser's connections and the
  server's query slots filled with histogram queries. On a 9.6 h recording
  the Syscall section showed its first charts after 0.15-0.25 s where it
  showed the loading page for 1.22-1.24 s, and finished 0.03-0.07 s later
  (1.27-1.29 s). A subgroup whose counters came back empty holds its
  histogram charts back until they land. An expanded chart opened while
  its query is in flight shows as loading until it lands. Compare mode and
  the static-site viewer are unchanged; a zoom refetch still sends every
  query at once, after any load of the section has finished.

- **metriken-query 0.40.** A strided histogram call
  (`histogram_heatmap(x, 60)`, `histogram_quantiles([..], x, 60)`,
  `histogram_count(x, 60)`) lays its windows at multiples of the stride,
  `(k·60 s, (k+1)·60 s]`, where 0.38 laid them from the first row read, and
  shares each row's delta among the windows its interval overlaps, in
  proportion to the overlap, where it counted the whole row in one window.
  A stride that is not a multiple of the row interval gives windows that
  each hold the same share of rows: on the 9.6 h bench recording, heatmap columns at a 1.5 s
  stride sum to 391k, 388k, 398k, 457k where whole rows gave 536k, 248k,
  528k, 268k. A heatmap cell or strided count can be fractional (to
  1/65536 of an observation; the raw-count tooltip shows a share under
  0.005 as 0). Across a gap in a series, the delta of the row after it is
  shared over the gap's windows, so a strided heatmap or percentile line
  shows a level band across an outage where it showed an empty stretch and
  one spike.
  A closed window's point is stamped at its close, the last window at its
  last row. A heatmap of a whole recording starts with a partial column
  unless the recording starts on a multiple of the stride. A strided
  instant query counts the stride ending at the instant, including the
  share of the row that straddles its opening, where `histogram_count`,
  `_quantiles` and the rest read back 300 s. A stride under 1 ns, or NaN,
  is a parse error, and one shorter than the row interval laying more than
  100 000 windows over the range is refused. A heatmap of all 16 `syscall_latency` series over the 9.6 h
  recording at a 60 s stride takes 0.3 s (release build, warm).

- **`rezolus view`'s cache budget defaults to an eighth of the memory
  available**: physical memory, or a Linux cgroup's limit when lower (as in
  a container), clamped to 500 MB–8 GB. It was a fixed 500 MB.
  `--cache-size-mb` and `REZOLUS_CACHE_MB` still set it. A budget smaller
  than a query's row groups gives no reuse: a heatmap of all 16 syscall
  classes over a 9.6 h recording took 2.9 s on every repeat at 500 MB and
  0.37 s at 2 GB (release build).

- **metriken-query 0.38.** An instant query of a histogram function
  (`/api/v1/query`, the static viewer's `query()`) reads back three of the
  histogram's row intervals, not 300 s: on a 9.6 h, 1 s recording an
  instant `histogram_mean` takes tens of milliseconds where it took about a
  second. A series with no observations in the last three row intervals has
  no value at the instant, where 0.37 answered with the last tick that had
  observations, up to 300 s earlier (on the bench recording,
  `tcp_connect_latency` at most instants). An instant `histogram_count by
  (op) (x)` now answers; on 0.37 it returned no series. A strided call
  (`histogram_count(x, 60)`) read back 300 s until metriken-query 0.40. The
  dashboards issue range queries and are unaffected.

- **metriken-query 0.37, metriken-storage 0.3, metriken-exposition 0.23,
  metriken-recorder 0.2, dendro 0.5, arrow and parquet 60, ratatui 0.30.**
  In metriken-query 0.37, `rate(x[range])` is the average rate over
  `[t - range, t]`; it was the per-step rate, the same as `irate`. The
  dashboards, the MCP tools' generated queries and examples,
  `extract-features` and the viewer's default counter query use `irate`,
  which is the per-step rate as before. Queries you wrote with `rate(x[r])`
  (KPIs embedded in a recording, `--queries` files, saved selections) now
  return the average over `r`, so a `recording check` can pass a spike
  shorter than `r` that it failed before; use `irate` for the per-step rate.

  Every series read from a `.dendro` carries its recording's
  labels (`source`, `host` and any `record --label`), a single recording's
  included, and a series' own value of one of those keys moves to
  `exported_<key>`; `mcp query` prints them. A chart's legend, the Query
  Explorer's included, names each series by the labels that differ between
  its series, so a label they all share, as a recording's labels do within
  one recording, is left out. A query over several recordings flattened
  into one view is answered from every recording, each series under its
  recording's labels; it was refused. The MCP tools still refuse a
  multi-recording archive without `--recording`, so an analysis never mixes
  two arms.
- `ext4_journal` times commit phases, handle start and checkpoints in
  nanoseconds instead of jbd2's jiffies; `phase="wait"` and
  `phase="request_delay"` are removed, `ext4_journal_handle_start_latency` is
  new, and recordings from before are not comparable. (#1461)

- Reduced overhead when many CPUs count the same cgroup, by giving each
  per-cgroup BPF counter 16 copies; with cgroup attribution off the maps are
  not created. (#1450)

- **`record`, hindsight and the live viewer record through `metriken-recorder`
  0.1.1.** Detection, the stream and scrape transports, restart metadata and
  the recording loop moved out of rezolus into that crate; rezolus keeps the
  CLI, its probe hook (how a Rezolus agent is identified and refused), the
  `-- cmd` run events, signals, exit codes and messages. Recordings, exit
  codes and messages are unchanged, except that a producer-restart warning
  is logged by `metriken_recorder` and names the endpoint URL. With it come
  metriken-storage 0.2.3 and dendro 0.4.2, which seal no segment past the
  seal policy's row cap, keep a group's rows when its schema returns to an
  earlier one, and keep an open segment's rows when hindsight's lookback
  evicts past its first row. That segment is sealed then and dropped whole
  once its last row is past the lookback, so a hindsight buffer whose
  lookback is shorter than the seal policy's `max_age` (300 s) keeps up to
  twice its lookback plus one interval; the 15 m default is unaffected.
  Before, such a buffer lost the rows of every open segment once its
  lookback passed the segment's first row, and a dump with a 6 s lookback
  held only a long table's occupant rows.

- **`record` detects an endpoint by its stream.** For a URL whose path is `/`
  or `/metrics/stream`, `record` subscribes to `/metrics/stream` first; a
  stream handshake means the endpoint is streamed. Otherwise it reads
  `/status`, then `/`: a Rezolus agent is refused as before (or retried on a
  5xx, a 408, a 429, a timeout or no connection), anything else that answered
  is scraped as Prometheus at `/metrics` once `/metrics` answers with a
  success (retried each tick until then), and an endpoint where neither
  answered is retried each tick. Before, `record` classified a `/` URL by
  `/metrics/binary` (a msgpack snapshot meant an agent, else Prometheus), so
  a producer serving `/metrics/stream` without `/metrics/binary` was scraped.
  `/metrics/binary` is no longer read. A URL with any other path is still
  scraped at that URL. `--endpoint url,protocol=stream` is new and records
  the endpoint from its stream only; `protocol=rezolus` and
  `protocol=msgpack` (the 5.x spelling) are accepted and mean the same, and
  no longer require `/metrics/binary` to answer. A `rezolus exporter`
  endpoint (its `/` banner reads `Rezolus <version> Exporter`) is now scraped
  at `/metrics` instead of refused. Detection of an endpoint that comes up
  mid-run runs off the tick, so one that accepts connections and never
  answers no longer delays the other endpoints' ticks. An endpoint that the
  run ends without recording, because it never became reachable or its
  metadata read had not returned, is named in a warning.
- **A streamed producer's handshake names its recording.** `source` is the
  user's `source=`, else the stream handshake's `source` label, else
  `rezolus` for a Rezolus agent, else a name made from the URL. Labels are
  `--label`, over `host` from a Rezolus agent's `/systeminfo`, over the
  handshake's labels; metadata is `--metadata`, over the recorder's own keys
  (`sampling_interval_ms`, `producer_epoch`, `role`), over a Rezolus agent's
  `version`, `systeminfo` and `descriptions` (each taken only when it is a
  JSON object, and only from an endpoint recognised as an agent), over the
  handshake's metadata.
  Before, the handshake's labels and metadata were dropped, any producer
  that answered `/metrics/binary` was recorded as `source=rezolus`, and a
  streamed producer without `/status` had no `version`. A restarted
  producer's new handshake metadata is written into its recording.
- **`--metadata` outranks every other source of a metadata key.** A
  `--metadata systeminfo=` or `descriptions=` used to be overwritten by the
  agent's own, at the start and again when the agent restarted; the user's
  value is now kept in both cases, as it already was for `version` and
  `source`. A Prometheus endpoint's HELP text no longer replaces a
  `--metadata descriptions=`. `--metadata producer_epoch=` and
  `producer_epochs=` are refused as usage errors (exit 2): the recorder
  writes both from the stream handshake and rewrote them on a restart.
- **An agent's metadata is read after its stream opens, concurrently, and
  bounded.** `/status`, `/systeminfo` and `/metrics/descriptions` were read
  one endpoint at a time, without a time bound, before the stream was
  opened. They are now read after the handshake, for every streamed endpoint
  at once, each bounded by the scrape timeout (twice the interval, between
  2 s and 10 s); a reply that names another producer epoch is discarded. An
  endpoint whose metadata does not arrive in time is recorded with its
  handshake's labels and metadata only, with a warning. For an endpoint that
  comes up mid-run the read runs off the tick, and its recording is added on
  the first tick after the read returns.
- **A refusal quotes the stream route's answer.** A refused agent's message
  says `<url>/metrics/stream answered HTTP 404` (or the content type, or the
  malformed handshake) rather than the full subscription URL with its query.

- Reduced overhead by giving each CPU its own slice of every BPF histogram,
  at about 4 KiB of memory per possible CPU per histogram. (#1445)

- Reduced overhead when threads with neighbouring thread IDs (TIDs) run on
  different CPUs, by indexing per-thread BPF arrays through a permutation of
  the TID. The `id` label and `x<n>` series-name suffix of `task_cpu_usage`
  are now that index, not the TID; select a thread by its `pid` label. (#1444)

- **A Prometheus endpoint's HELP text is its recording's `descriptions`.**
  The scrape's `# HELP` lines (first seen wins) are written into the
  recording's `descriptions` metadata, the key an agent recording fills from
  `/metrics/descriptions`, and updated when a later scrape brings a new
  metric name. 5.x wrote them into parquet output only, and the `.dendro`
  path dropped them.
- **`rezolus-capture` records one `.dendro`.** `scripts/rezolus-capture` and
  the Docker image's `rezolus-capture` ran `rezolus record` to parquet, which
  6.x refuses, so every capture failed. A capture is now one `rezolus record`
  with an `--endpoint` per source, written to `capture.dendro` in
  `--output-dir`: a recording per endpoint, `source=rezolus` for the agent
  and `source=<NAME>` for each service. The command-line options are
  unchanged; `--source rezolus` is refused, since it names the agent's
  recording.
- **`rezolus-capture` with `--endpoint` loses the merged dashboard.** The
  viewer opens a capture made with `--endpoint` as an A/B comparison of the
  agent's recording against the first service's, with further services as
  extra overlays; no dashboard queries the service metrics. Service KPI
  dashboards (vllm, valkey, sglang and the other templates) are not shown,
  because the template queries select a `source` label that `.dendro`
  series do not carry. `capture.parquet` and the per-endpoint
  `agent-<port>.parquet` / `<source>-<port>.parquet` copies are no longer
  written. For the merged dashboard with service KPIs, use the 5.x
  `rezolus-capture` (the v5.27.0 script or Docker image), which writes a
  merged `capture.parquet`.
- **`record --endpoint url,role=...` is kept in the recording.** The role is
  the `role` key of that endpoint's recording metadata; `--metadata role=...`
  overrides it. Before, only parquet output carried it.

- **Exact label listings for sparse long tables** (metriken-storage 0.2.1).
  A long table whose columns have values for only some of its occupants, as
  a metriken producer's single group of plain metrics and labelled counter
  families does, listed every column once per occupant, with the occupant's
  labels: `describe-metrics`, the viewer's metric catalog and anomaly
  detection's label hints showed series that do not exist. Recordings made
  with this version list only the series that have values. Archives recorded
  before keep the inflated listing; queries were always right.

- **The agent serves `/metrics/stream` under metriken's names:**
  `Content-Type: application/vnd.metriken.replication.v1+dendro` and the
  `x-metriken-frame-interval`, `x-metriken-update-floor` and
  `x-metriken-layout` headers, in place of `vnd.rezolus.replication` and
  `x-rezolus-*`. The route is metriken-exposition 0.22.0's
  `StreamSubscription`; the agent keeps the pass it hands it (the sampler
  refresh behind the TTL, and the pass cache `/metrics/rows` shares). Frames
  are unchanged, pinned by `tests/fixtures/stream-frames/`. A stream that
  stops being read waits with no lock held, and on resuming skips to the
  current pass and logs a `seq` gap.
  - `record`, `hindsight` and the live viewer accept both sets of names, so
    agents from 5.21.0 through 6.0.0-alpha.44 are still streamed.
  - **Older consumers of the stream refuse this agent's stream**, naming the
    content type they got: `record --stream` from 5.21.0 through 5.26.x, and
    `record` to `.dendro`, `hindsight` and the live viewer from 6.0.0-alpha
    builds through alpha.44. Drop `--stream` (5.x), upgrade, or record to
    `.rez` or parquet with 5.x, which scrapes `/metrics/binary`; that path
    is unchanged.
  - A subscriber reads the time it woke before waiting for the pass lock,
    so subscribers at one boundary share one sampling pass. Before, one
    queued behind a pass longer than the TTL found it stale and sampled
    again.
- **`tcp_packet_latency` no longer creates and deletes a map entry per
  segment.** Each data segment inserted the socket into a hash map and each
  read deleted it, taking a bucket lock both times. On a production host
  where both programs ran about 4.4M times per second, `bpf_stats` measured
  521 ns per run for `tcp_probe` and 386 ns for `tcp_rcv_space_adjust`. The
  start times are now kept in a fixed 4 MiB array of 2^19 slots indexed by a
  hash of the socket's address, with no allocation or lock; each slot is one
  64-bit word, so a slot is never read half written. The sampler attaches
  `tp_btf` programs where the kernel has `bpf_skc_to_tcp_sock` (5.9+) and
  `raw_tp` programs otherwise. In a 6.12 KVM guest over loopback (MTU 1500,
  valkey-benchmark 4 KiB SET/GET, 48 clients, one run per arm), ns per run
  for `tcp_probe` / `tcp_rcv_space_adjust` went from 326 / 523 to 226 / 325
  with `tp_btf` and to 264 / 283 with `raw_tp`.
  - Only an in-order data segment (sequence number `rcv_nxt`) starts a
    sample. Duplicate, out-of-order and beyond-window segments did before,
    and one that was dropped could leave a start time that a much later read
    recorded. An in-order segment that the kernel then drops (checksum or
    PAWS failure, zero receive window, receive memory exhausted) still
    starts one, as before.
  - A segment is not timed when its socket's slot holds another socket's
    start time, either for unread data or for a socket no read clears, such
    as an MPTCP subflow. The miss rate grows with the number of sockets
    holding a start time at the same moment, K, and is about K / 2^19 when
    socket addresses spread evenly over the slots. If a later in-order
    segment arrives before the read, the sample starts there and is shorter
    than the data's wait. The new data in a partial duplicate is not timed.
  - Against an exact per-socket reference on the same segments, the slot
    array missed 0.20-0.22% of samples at 2000 clients; p90, p99 and p99.9
    fell in the same buckets, and p50 read one bucket high in three of four
    runs at 2000-5000 clients.
  - The old map dropped samples past 10240 sockets holding a start time at
    once.

- **The `.rez` format and the archive reader and writer are
  `metriken-storage`'s** (0.2.0, feature `rez`), and queries over an archive
  are `metriken-query` 0.36.0's. metriken-archive and metriken-segment are no
  longer dependencies; raw msgpack conversion (`recording convert`) is
  metriken-storage's `convert`.
  `crates/rez` keeps `RezReader` and `LiveReader` and re-exports the rest at
  its old paths.
- **An agent restart is written into the recording's metadata.** When the
  agent restarts mid-recording, an archive written by `record` or `hindsight`
  is amended once the first row from the new process is
  staged: `producer_epoch` becomes the new epoch, `producer_epochs` (dendro's
  key) lists every epoch with the timestamp of its first row and the version
  it reported, and `version`, `systeminfo` and `descriptions` are read again
  from the new process. `descriptions` keeps the earlier process's entries,
  so the rows from before the restart stay described. A `version` set with
  `--metadata` is kept. Before, the metadata described only the process the
  recording opened with, and a log warning was the only record of the
  restart.
- The viewer asks for about one bucket per 4 pixels of chart width (75 to
  300) instead of one per pixel, and the server rounds the width up. On a
  9.6-hour recording at 1 s in a 1,600-pixel window, a half-width chart of
  the whole recording is drawn in 5-minute buckets instead of 1-minute
  ones, and an hour in 20-second buckets. A window holding no more samples
  than the budget (about 200 s at 1 s there) is drawn at the query step, one
  point per sample with no min/max envelope; before, that took 48 samples or
  fewer. Between the budget and about five times it, a bucket can hold
  fewer than 5 samples (2 at a 1 s interval, up to twice the budget), where
  before it held at least 5.
- A zoom can narrow to five samples at the recording's interval, whatever
  the spacing of the points drawn. It was five of the points drawn, which
  with 5-minute buckets would have stopped a zoom at 25 minutes.
- A per-entity heatmap (a `by (id)` panel such as per-CPU busy %) is
  fetched at the line charts' bucket width instead of at every sample, in
  compare mode too. A rate is fetched at that step in grid mode, each cell
  the average rate over its column; a column the recording does not cover
  in full is left out. A ratio of rates such as IPC is the ratio of the
  column's totals. A gauge (the GPU panels) is fetched in display mode,
  each cell its column's median. On a 9.6-hour recording at 1 s, a per-CPU
  panel's response is about 44 kB instead of about 17.5 MB.
- A line that averages over `by (id)` (`avg(sum by (id) (...))`) is drawn in
  display mode with its band, like other line charts.
- `rezolus view` runs dashboard queries on a pool of threads, several at
  once, instead of one at a time on the server's single worker. A slow query
  now occupies one slot, and other requests are answered while it runs.
  `--query-concurrency N` (or `REZOLUS_QUERY_CONCURRENCY`) sets how many run
  at once; the default is half the CPUs, from 2 to 8. On a 16-CPU host,
  loading every section of a 9.6-hour recording's dashboard, the median
  request waited 267-287 ms instead of 1.2 s, and the 90th percentile
  2.4-2.6 s instead of 14-15 s. Peak memory over the load was 2.4 GB,
  against 2.8 GB before (one run each).
- `rezolus view` builds a Save as Report download in a query slot, under
  `--query-concurrency`. Working out which columns to keep, which on an
  archive opens the tables each saved query touches, ran on an async worker
  before the build, and the build ran on the blocking pool outside any slot.
  A save now waits for a free slot, as a query does.
- `rezolus view` loads an upload, a URL or an attached experiment, and
  rebuilds the dashboards on a detach, on the blocking pool under
  `--query-concurrency`; these requests now wait for a free slot as queries
  do. They ran on one of the server's two async workers and held it while
  the file was written to disk, opened, checksummed and checked against its
  KPIs.
- metriken-query 0.34.7 and metriken-archive 0.3.5. The viewer's charts are
  reduced to their point budget while the query is computed, instead of
  after the full-resolution result is built. A `sum`, `avg`, `min`, `max`
  or `count` chart of `rate`/`irate` over a .dendro recording also
  holds each group's points only until they are final.
  - On a 9.6-hour recording of a per-task group stored long, an
    unaggregated `irate(...)` chart took 9.4 GB of memory and 6.0 s, and
    takes 434-486 MB and 2.7 s. A `sum by (comm)` chart took 1.3 GB and
    takes 387-399 MB. Stored wide, the `irate(...)` chart took 8.7 GB and
    takes about 1.5 GB; there, decoding the table is most of the memory.
  - On the same recording, a query over a small part of the range, whose
    time is mostly spent opening the table, takes 1.5-1.65 s instead of
    2.7-3.0 s.
  - A series that stops for more than 10 × max(its sample spacing, the
    step) and then resumes, as after an agent restart, makes a grouped
    chart's query fall back to evaluating per series. That uses as much
    memory as before, and up to about twice the time.
  - A `sum` or `avg` chart's values can differ from 0.34.6 in the last bits
    of a float when a series with fewer than nine samples stops more than
    ten sample intervals before the end of the range.

  Every series of a chart now shares one set of buckets, set by the chart's
  time range and step:
  - a chart whose range fits its point budget at its step shows every
    point, at any interval;
  - otherwise the buckets are one of the round widths, from 1 s up, so a
    series covering part of the range is bucketed like the rest and can get
    wider buckets than before;
  - a bucket holding one point shows it at its own time;
  - a chart in raw rate mode, or of a histogram, keeps to its point budget.
- metriken-query 0.34.4 and metriken-archive 0.3.4. `rate`/`irate`, alone
  or under `sum`/`avg`/`min`/`max`/`count`, are computed in one pass over a
  `.dendro` archive's columns, including in an expression that
  reads several tables of one recording (such as
  `sum(irate(cpu_usage[5m])) / cpu_cores`). Raw rate mode and a query
  joining samplers of different cadence are computed per series as before. On a 9.6-hour recording of a per-task group at a 1 s
  step, a warm `sum(irate(...))` took 9.7 s (table stored long) or 14 s
  (stored wide) and takes 0.89 s or 1.18 s; `sum by (comm)` took 10-11 s or
  15 s and takes 1.07 s or 1.38 s (these "before" figures are from the same
  runs; the 0.34.2 entry below was measured under heavier load). A
  `sum(irate(...))` over the table stored long peaks at 0.98 GB of memory
  footprint, against 1.65 GB. `sum(irate(task_cpu_usage[5s])) / cpu_cores` over
  the table stored wide took 23-44 s end to end in `rezolus mcp query` and
  takes 10.8-13.7 s, on a loaded machine. A query reads segments on up to 8 threads (one
  per core, at most 8); the browser viewer runs a query on one thread. An
  aggregate's values and bands can differ in the last bits of a float from
  earlier releases, between hosts with different core counts, and between
  the server and browser viewers.
- A live viewer, or a viewer following an archive file, reuses each
  table's state across reopens (metriken-query 0.34.1, metriken-archive
  0.3.3): the first query after a reopen reads only the segments sealed
  since and the live tail. On a 30-minute recording of a per-task group it
  took 2.7x a warm query, and takes 1.2-1.3x. Reuse needs the file at the
  path to be the one the previous reader read; a file renamed over it, as
  `recording filter` without `-o` does, is read afresh.
- metriken-query 0.34.2: an aggregation with several groups advances them
  together. On a 9.6-hour recording of a per-task group, `sum by (comm)
  (irate(task_cpu_usage[5s]))` took 192 s (wide) or 423-603 s (long) and
  takes 13-16 s.
- **`scheduler_runqueue` and `cpu_migrations` are one sampler,
  `scheduler`,** with one program on `sched_switch` instead of two (and
  `cpu_perf`'s, which stays a sampler of its own). The migration count
  reuses the runqueue part's read of the next task's cgroup. Its parts are
  the options `runqueue` and `migrations` in `[samplers.scheduler]`, both on
  by default, and `cgroup_attribution` applies to both. A config that still
  names `[samplers.scheduler_runqueue]` or `[samplers.cpu_migrations]` is
  read as `[samplers.scheduler]` with a warning. The merged sampler has one
  `cgroup_attribution`; if the two old samplers resolved it differently
  (counting `[defaults]` and their default of on), it is on, so no series
  that was exported disappears, and the difference is reported. Metric names
  are unchanged; the `sampler` label, `rezolus status`,
  `rezolus_bpf_run_time` and the archive table keys now say `scheduler`, and
  the migration acquisition groups are named `scheduler_migrations` and
  `scheduler_cgroup_migrations`. `extract-features` reads the old sampler
  labels as `scheduler`, and its record is schema version 5.
- **`blockio_requests` and `blockio_latency` are one sampler, `blockio`,**
  with one program on `block_rq_complete` instead of two; the request's
  fields are read once, as direct loads where the kernel has BTF. Its parts
  are the options `requests` and `latency` in `[samplers.blockio]`, both on
  by default. A config that still names `[samplers.blockio_requests]` or
  `[samplers.blockio_latency]` is read as `[samplers.blockio]` with a
  warning. Metric names are unchanged; the `sampler` label, `rezolus status`,
  `rezolus_bpf_run_time` and the archive table keys now say `blockio`.
  `extract-features` reads the old sampler labels as `blockio`, and its
  record is schema version 4.
- **`syscall_counts` and `syscall_latency` are one sampler, `syscall`,** with
  one program on `sys_enter` and one on `sys_exit` instead of two programs on
  `sys_enter`; each program on a hook costs its own dispatch, measured at
  38 ns per syscall on bare metal. Its parts are the options `counts` and
  `latency` in `[samplers.syscall]`, both on by default. A config that still
  names `[samplers.syscall_counts]` or `[samplers.syscall_latency]` is read
  as `[samplers.syscall]` with a warning, keeping each old section's
  `enabled` and `cgroup_attribution`; a section naming no known sampler is
  now reported. Metric names are unchanged. The `sampler` label, `rezolus
  status`, `rezolus_bpf_run_time` and the archive table keys now say
  `syscall`. `extract-features` reads the old sampler labels as `syscall`,
  and its record is schema version 3.
- `scheduler_runqueue`, `cpu_migrations` and `cpu_usage` read the task's pid,
  state and CPU times as direct loads from BTF pointers instead of
  `bpf_probe_read_kernel()` calls; on bare metal (EPYC 4564P, 6.12)
  `scheduler_runqueue` costs 91–95 ns per run against 136–142 ns, and
  `cpu_migrations` 38–39 ns against 56–59 ns. `syscall_latency` attaches to
  `sys_enter` and `sys_exit` as raw tracepoints (`tp_btf`, or `raw_tp`
  without kernel BTF) instead of the `raw_syscalls` tracepoints: with no
  other syscall tracer on the host, its overhead per syscall falls from about
  346 ns to 226 ns in a KVM guest. Where another tool keeps classic programs
  on those tracepoints, classic programs share its trace-record build and
  dispatch and the raw tracepoints do not: with `bpftrace` attached, the two
  syscall samplers together cost about 121 ns per syscall more than on
  classic tracepoints, against 82 ns less without it. The series are
  unchanged.
- **Groups of slots travel long on the agent's stream.**
  `/metrics/stream?layout=long` sends a group whose metrics are all counter
  or gauge groups (per task, per cgroup, per CPU) as values keyed by
  occupant, with each occupant's labels sent once on `<group>/occupants`
  when it first appears to the subscription. A task starting or exiting
  then costs that task's labels instead of the group's whole member list.
  The agent builds the wide snapshot only when something reads it, so an
  agent that is only streamed long never builds it. `record` to `.dendro`,
  hindsight and the live viewer ask for the long layout. An agent that does not serve the long
  layout is recorded through the wide one, and `record` logs that when it
  connects. The response names the layout served in `x-rezolus-layout`.
- metriken-query 0.34.0, metriken-exposition 0.21.4, metriken-archive 0.3.2
  and dendro 0.3.4. A query whose metric is in the recording but matches
  nothing in the range answers empty instead of "metric not found";
  `rezolus mcp query` prints "No series matched." for it, and a metric read
  with a function for another kind (`rate()` of a gauge) says which kind it
  is. Archives written through the new writer carry a heartbeat while their
  writer runs.
- Every sampler with a per-event cgroup path reads the task group once, as
  direct loads from a BTF task pointer where the program is `tp_btf`,
  `fentry` or `fexit`, instead of through up to seven `bpf_probe_read_kernel()` calls:
  `cpu_usage`, `cpu_migrations`, `cpu_perf`, `cpu_tlb_flush`,
  `scheduler_runqueue`, `ext4_ops`, `xfs_log` and `memory_pagecache`, after
  `syscall_counts`. `cpu_tlb_flush` attaches to `tlb_flush` as `tp_btf`
  where the kernel has BTF. On bare metal (EPYC 4564P, 6.12),
  `scheduler_runqueue` costs 137–139 ns per run against 280–328 ns before,
  and `cpu_tlb_flush` 37–45 ns against 146–162 ns. The series are unchanged.
- `syscall_counts` attaches to `sys_enter` as a raw tracepoint (`tp_btf`,
  or `raw_tp` without kernel BTF) instead of the `raw_syscalls/sys_enter`
  tracepoint, and its per-cgroup path reads the task group once, as direct
  loads from a BTF task pointer on kernels from 5.11. On bare metal (EPYC
  4564P, 6.12) the program costs 33–39 ns per syscall with
  `cgroup_attribution` on, down from 140–151 ns. The series are unchanged.
- `cpu_usage`, `cpu_migrations`, `cpu_tlb_flush`, `scheduler_runqueue` and
  `syscall_counts` honour `cgroup_attribution`, on by default as for
  `cpu_perf`, so nothing changes unless a config sets it. With
  `cgroup_attribution = false` the per-cgroup path (the task-group read, the
  new-cgroup check and the per-cgroup adds) is folded out of each sampler's
  hook and the `cgroup_*` series are absent; host-level series are
  unchanged. A config that already sets `[defaults] cgroup_attribution =
  false` now reaches these five samplers as well. `docs/metrics.md` lists
  every sampler with the option and its default. The cgroups view finds
  cgroup names from any per-cgroup series when `cgroup_cpu_usage` is absent.
- metriken-query 0.33.6 and metriken-archive 0.3.1. A segment's decoded
  blocks are cached by its bytes, so a reader reopened over the same archive
  finds the blocks the previous one decoded. A query range that starts or
  ends at a sample's own time now includes that sample; the range's `f64`
  seconds could convert to up to about 130 ns past it, which on a two-second
  recording made `rate()` at a 1 s step return nothing, reported as the
  metric not being found.
- **The viewer's live mode records the agent's stream.** `rezolus view
  http://agent:4241` (and `--tui`) subscribes to `/metrics/stream` and writes
  it into a temporary `.dendro` archive, read as it grows, instead of polling
  `/metrics/binary` into memory and keeping every raw snapshot body for
  saving. Save capture downloads a copy of the archive as
  `rezolus-capture.dendro`. Reset starts a fresh archive; before, reset left
  the view empty, because polling continued into the store it replaced. If
  the recording stops (the agent refuses a reconnect, or a write fails), the
  page says why and stops presenting the view as recording.
- **Hindsight records the agent over its replication stream.** It subscribes
  to `/metrics/stream`, as `record` to a `.dendro` does, instead of scraping
  `/metrics/binary`, whose every body carries every acquisition group's full
  schema. Rows carry the agent's own timestamps. An agent that cannot serve the
  stream (older than 5.21.0, or a V2 agent) is refused at startup with its
  version; a stream that drops is reconnected after one interval, and a
  reconnect to a restarted agent is logged. When an agent answers a reconnect
  but can no longer serve the stream, hindsight writes the buffer to a
  timestamped file beside `output`, where a SIGHUP capture goes, and exits
  with status 1. `[general] source` still names the agent.
- A recording streamed into a `.dendro` decodes each row once. The recorder
  decoded every streamed payload twice, once to rebuild the row endpoint's
  envelope and once to build the snapshot the writer takes. At 100 ms under
  process churn on a 32-core host the recorder used 24–27% less CPU over four
  180 s windows (25.9–37.4 s → 19.6–27.5 s).
- The agent converts each acquisition group's schema for `/metrics/stream`
  and `/metrics/rows` once per schema change instead of on every sampling
  pass (metriken-archive 0.3.0's `SchemaCache`), and a changed schema is put
  into the already-encoded row rather than the row being decoded and encoded
  again. On the `v3_build_cost` registry (a 2,500-task group) encoding a pass
  for the stream fell from 20.4 ms to 0.29 ms at the median, and to 4.2 ms on
  a pass where the task group's membership changed; encoding the same pass
  for a scrape takes 9.7 ms. On a 32-core host under process churn, a
  streamed agent's CPU fell 31% at 1 Hz and 38% at 10 Hz, to within 0–16% of
  the same agent scraped (it had cost 50–70% more).
- The agent's V3 router resolves each metric module's sampler once and
  caches it, and looks acquisition groups up with foldhash. Attribution was
  a scan of every sampler module, run for every metric twice a pass. On the
  `v3_build_cost` registry the build's median fell about 4% on cache-hit
  ticks and 2.5% on membership-change ticks.
- metriken-exposition 0.21.3: the V3 group builder uses foldhash for its
  per-pass maps and sizes a cache-hit group from the lookup it already made,
  rather than looking each member up a second time.
- **A `.dendro` records Rezolus agents by stream only.** `record -o
  out.dendro` (and a bare `rezolus record`, whose default output is a
  `.dendro`) subscribes to each agent's `/metrics/stream` and scrapes each
  Prometheus endpoint; one run can mix both, each endpoint its own recording,
  with streamed rows and scrapes committed through one archive writer once
  per tick. An agent that cannot serve the stream (older than 5.21.0, a V2
  agent, or a handshake that does not decode, such as a replication protocol
  version mismatch) is refused with its version and never scraped instead:
  at startup the run is refused before the archive is created, and an agent
  that comes up later, or that answers a reconnect with such a refusal, is
  left out from then on (a reconnecting agent's recording keeps the rows it
  had and is finalized) while the other endpoints keep recording, the
  archive is finalized, and the run exits 1. Record such an agent with a
  5.x recorder, which scrapes. An agent that answers but whose stream
  fails with an error that can change (a 5xx, a handshake timeout) is retried
  each tick. A wrapped command that exits on its own waits for each agent's
  frame covering the exit, at most one interval plus the scrape timeout
  (twice the interval, between 2 s and 10 s). A run whose every endpoint has
  been refused ends there, and each refused endpoint is named at the end.
  `record --stream` is removed: `record` writes `.dendro`, which records
  agents by stream, and passing `--stream` is an unknown-argument error
  (exit 2). The identity index that a 5.x `.rez --stream` stored beside its
  rows is gone: the
  agent no longer sends `Frame::Index` on `/metrics/stream` (every rows frame
  names dendro's `NO_INDEX_STATE`), no longer keeps the slot-change broadcast
  or index history, and `/status` drops `index_resyncs`. Slot identity
  reaches a `.dendro` through each group's schema, `__uid__` included.
  `recording upgrade` copies that index from a `.rez` a 5.x `record
  --stream` wrote into the `.dendro` as caller rows, and it is not read:
  each slot column keeps the labels its segment was built with, which
  splits reused slots where a member carries its labels, and each indexed
  table logs a warning. 5.x keeps `.rez --stream` on
  `release/5.x`; a 5.x `record --stream -o out.rez` against a 6.0 agent
  receives no index frames, so it no longer records an identity index. A 6.0
  recorder ignores a 5.x agent's index frames.
- The agent's V3 snapshot is built by metriken-exposition's `GroupBuilder`
  and its `/metrics/stream` frames by metriken-archive's `FrameProducer`.
  The agent supplies a router (`src/agent/exposition/http/router.rs`) that
  does what `create_v3` did itself: the `log_` filter, sampler attribution,
  `acq_group` routing against `ACQUISITION_GROUPS`, each group's membership
  rule and window source, and the `sampler` label. Groups, schemas, schema
  hashes, values, windows and snapshot metadata are unchanged; a test pins
  them over nine ticks of slot assignment, release and metadata changes.
  `/metrics/rows`, V2 snapshots, the stream's route, timer and per-subscriber
  filters are unchanged. The hit-path eviction no longer logs at `debug`.
- `cpu_perf` honours `cgroup_attribution` (on by default for this
  sampler). With `cgroup_attribution = false` its `sched_switch` program is
  not loaded, so nothing reads the PMU per context switch; `cpu_cycles` and
  `cpu_instructions` are read per CPU at scrape time and the `cgroup_cpu_*`
  series are absent. On a KVM guest with an emulated PMU the program's two
  counter reads measured 20 µs per context switch. A `[defaults]
  cgroup_attribution = false` now reaches `cpu_perf` too.
- `cpu_usage` no longer exports `task_cpu_usage` unless its section (or
  `[defaults]`) sets `task_attribution = true`. The per-task accounting the
  host and cgroup totals are computed from still runs; what is dropped by
  default is the export: the task-metadata and task-exit events, the walk of
  the per-pid map's populated slots each time a snapshot is served, and one
  series per thread in recordings.
- Slot identity and the producer epoch come from metriken 0.11.2
  (`metriken::group::SlotIdentity`, `metriken::epoch`), which took them from
  the agent; `__uid__` minting, the no-op on a re-announced label set, and
  the epoch's format are unchanged. Samplers declare the metrics a slot
  spans as one list per acquisition group (`SlotIdentity::grouped`).
- `ext4_ops` and `xfs_log` attribute to cgroups only when their section (or
  `[defaults]`) sets `cgroup_attribution = true`. The per-cgroup path was
  measured at half the end hook's cost (265 of 535 ns), so it is off by
  default; when off, the `cgroup_ext4_*` and `cgroup_xfs_log_*` series are
  absent and the path is folded out of the loaded program.

### Added

- **Memory metrics on macOS.** `memory_meminfo` and `memory_vmstat` now run
  on macOS, unprivileged. They set `memory_total`, `memory_free`,
  `memory_available`, `memory_cached`, `memory_anon`, swap total and free,
  `memory_page_faults` and swap in and out, so the memory dashboard's
  capacity, used and swap charts draw on a Mac. `memory_available` has the
  Linux meaning: free, file-backed and purgeable pages. macOS-only metrics
  cover wired memory, purgeable and speculative pages, the compressor
  (`memory_compressed`, `memory_compressed_stored`, compressions and
  decompressions) and file page-ins and page-outs. The gauges come from
  sysctls; the counters and wired memory come from `host_statistics64`, which
  macOS rate-limits for third-party processes, and a read that is not newer
  than the last one published is dropped. The memory dashboard draws the
  Anonymous chart whenever `memory_anon` is recorded and adds a Wired and
  Purgeable subgroup and a Compressor subgroup.
- **A systemd unit's cgroups can be selected as one entry.** The cgroup
  selector lists each `*.scope` or `*.service` unit with two or more recorded
  cgroups that name it (a libvirt guest's `vcpuN` and `emulator` threads, a
  service's child cgroups) as a group above its members. The agent records a
  cgroup more than three levels deep by its last three path components
  (`.../machine-qemu-2-vm1.scope/libvirt/vcpu0`), so a group holds the unit's
  own cgroup and those up to two levels below it; a cgroup further down is not
  in it. A selected group plots one line per individual chart, from its own
  query over the members, so a group's IPC is its summed instructions over its
  summed cycles; the aggregate charts exclude its members. While a group is
  selected, an entry that shares a cgroup with it cannot be selected, and
  selecting a group replaces the selected entries it holds. The link carries a
  group as its unit path with a trailing slash
  (`cgroup=.../machine-qemu-2-vm1.scope/`), and the members are found again in
  whichever recording opens it. A key that carries more or less of the unit's
  path than that recording's key for it (`/machine.slice/...` against `.../`)
  resolves to it when exactly one group matches, and a link that holds a group
  and one of its members is rewritten to the group. A group with a member the
  selector cannot quote is not selectable. A group's members are found when
  the section opens, so in a live view a cgroup created under the unit after
  that is counted in the aggregate until the section is opened again. A pinned
  chart that draws a group is not compared in A/B mode. (#1544)
- **`process_cpu_usage`: CPU time per process.** With
  `process_attribution = true` (off by default, independent of
  `task_attribution`), `cpu_usage` exports one series per thread
  group, labelled `tgid`, `comm` (the group leader's, relabelled on exec and
  on the leader's renames) and `cgroup`. It counts every thread of the
  process, including threads that have exited and threads with no series of
  their own, so a process's total no longer depends on how many threads it
  created. Its series ends within one snapshot interval of the process's
  last thread exiting. It costs 64 MiB of kernel memory (two per-pid
  arrays) and a 256 KiB ring, a metadata event per new process and per
  leader rename, an exit event per process when `task_attribution` is off,
  the `task_rename` program (one run per thread rename), and two marker
  reads and a counter add per accounting event that adds CPU. A process whose threads
  run on many CPUs contends on its counter: with 48 threads of one process
  on a 56-vCPU host at HZ=250, each run of the `cpu_usage` BPF programs
  took about 310 ns, against about 200 ns with 48 one-thread processes and
  170 ns without the counter. (#1490)

- **`drive_temperature_sensor`: NVMe Temperature Sensors 1 to 8.**
  `drivehealth` decodes them from the SMART/Health log it already reads
  (bytes 200 to 215), labelled `sensor={1..8}` and with the drive's labels,
  on Linux and macOS. A sensor the controller does not implement has no
  value. These are the per-sensor readings `hw_sensors` stopped collecting
  when it left `nvme` hwmon chips to `drivehealth`. (#1526)
- **`record --endpoint url,host=NAME` sets the recording's `host` label.**
  `record` names labels that a Rezolus agent's metadata brought too late to
  put on the recording (metriken-recorder's `LabelsNotApplied`), with the
  `--endpoint` that sets them on the next run: the argument as typed with
  `host=` (and `source=`/`arm=`) added in place of any it gave, any other
  label as `--label`, and with several endpoints the one it replaces, e.g.
  `warn: svc: its host label arrived late and is not on the recording;
  record again with --endpoint http://a:4241,source=svc,host=web-01`.
  Hindsight and the live viewer log the labels.
- **A new output never replaces a file.** The `recording` tools stage each
  output in `.<output>.rezolus-staging.<pid>.<random>` beside it and link it
  into place, refusing an output that appeared meanwhile (an in-place
  `filter` and `convert --force` replace, as asked). `filter x.dendro -o y`
  now refuses a `y` that exists before reading anything, where it replaced
  it; `-o` naming the input itself (`./x.dendro`) filters in place. A killed
  command leaves that directory behind. Save as Report in the static site
  refuses a recording over 1 GiB (it holds about three copies in memory),
  naming `rezolus view`.
- **Gauge histograms are listed.** A `.dendro` holding gauge histograms
  (metriken 0.11.4's `GaugeHistogram`) shows them in the viewer's metric
  browser (as histograms, charted through the histogram functions) and in
  `mcp describe-metrics` (their own section), through every reader the
  viewers use (`RezReader`, `LiveReader`, and the combined and per-recording
  views). `detect-anomalies` and `extract-features` do not cover them.
- **`record --endpoint url,template=FILE` attaches a KPI template to an
  endpoint's recording.** The file, a `ServiceExtension` JSON as `recording
  annotate --queries` takes, is written into that recording's metadata as
  `service_queries`, so the viewer shows the service's KPI section for a
  scraped Prometheus endpoint, which has no handshake to carry one. A config
  file's `[[endpoints]]` entry takes `template = "FILE"`, relative to the
  config file's directory. The file is read and checked before recording:
  an unreadable file, one over 1 MiB, JSON that is not a `ServiceExtension`,
  or an unknown `unit_system` refuses the run (exit 2) and writes nothing.
  `recording annotate --queries` refuses a file over 1 MiB too. On a streamed producer, `template=` replaces the
  template its handshake sends, and the recorder logs that at startup.
  cachecannon and llm-perf send their own from their next releases; the files in
  `crates/dashboard/templates/` stay as the fallback for recordings that
  carry none (docs/usage.md, "KPI templates").
- **The agent sends its section template, and both viewers render the
  built-in sections from a template.** A Rezolus agent's `/metrics/stream`
  handshake carries the Rezolus section template under the metadata key
  `dashboard`, with `revision` set to `rezolus <version>`. `record`,
  hindsight and the live viewer keep it in the dendro source's metadata. The
  viewers (`rezolus view`, its terminal UI, and the WASM viewer) render the
  Overview, Exceptions, CPU and the other built-in sections by expanding a
  section template rather than calling the Rust section generators: the
  recording's `dashboard` when it is at most 1 MiB, parses, and has a section
  the viewer can show, else the template the viewer embeds. The navigation
  comes from the chosen template, less any section at a route the page renders
  itself (`/about`, `/systeminfo`, `/metadata`, `/notebook`, `/selection`,
  `/report`) or of more than one segment, which the page cannot reach; those
  are named in a notice. The sidebar finds the Overview and Exceptions by
  route, so a template may rename them, and the page mounts the query
  explorer and its own views (system info, metadata, notebook, selection,
  report) by route, so a template section named like one of them is an
  ordinary section.
  A recording without the key (one upgraded from a `.rez` or parquet, or a
  recording of an older agent) renders the embedded template, which draws
  the same JSON as the generators did. A `dashboard` that is too large or
  does not parse renders the embedded template, with a warning once per
  distinct message (the server's log; the browser console, from
  `init_templates`). `rezolus view --registry-templates` and
  `WasmCaptureRegistry::set_prefer_registry_templates` now also select the
  embedded section template over the recording's. A section the template
  cannot expand (a limit, or a parameter the viewer did not set) renders
  empty, with the reason in its `metadata.template_error`, logged once by
  the server and by the page; the other sections are unaffected. In a
  comparison, the baseline's template renders for both captures. The cost is
  the handshake: about 106 KB once per subscription, and the same once per
  source in each archive written from the stream. The viewers'
  `/api/v1/file_metadata` (and the WASM `file_metadata_json`) leave the key
  out; `rezolus recording metadata --json` prints it, as it prints every
  key. The WASM bundle grows by 403 KB, from 7,273,651 to 7,677,243 bytes
  (99 KB gzipped, 2,389,446 to 2,488,591): the embedded template and the
  expander.
  Rendering a section from the template took the same time as the
  generators on a 9.6-hour recording (all built-in sections, warm: 0.48 ms
  against 0.45 ms; cold: 83 ms for both).
- **Section templates, schema 1.** `dashboard::section_template` reads a
  JSON document describing dashboard sections (groups, subgroups and plots,
  with `when`, `repeat`, `split` and `rate_mean` nodes over predicates on
  the metric catalog) and expands it against a recording into the same
  `View` JSON the section generators produce. Parsing reads `schema` first
  and refuses a schema above 1 before anything else. It then refuses, in
  every branch whether or not a catalog would expand it: an unknown field or
  enumerated value, an array where an object belongs, a repeated key, an
  unknown or malformed `${..}` name, a catalog label value outside a
  double-quoted string in a query (or after a `\` or in a `#` comment), a
  let that uses another let, an undeclared parameter, more than 64 sections,
  a malformed or duplicate route, a section at `/query` (the query
  explorer's, placed with `{"builtin": "query"}`), an array or object of
  more than 4096 entries, a string over 16 KiB or holding a control
  character, a name or a predicate label name over 64 bytes as written, and
  a `distinct_gt` `int` label that is not one of its `by` labels (of which
  there are at most 16).
  Parsing is linear in the document's size, which it does not limit, so a
  caller reading untrusted templates caps it first. Each section's expansion
  is bounded on its own: 20,000 nodes; 4 MiB of output counted in memory
  (strings, checked before each piece is appended, plus copied metadata, the
  group, subgroup and plot structs, and the copies `histogram_rate_mean`
  makes; a control character from the catalog counts as its six-byte JSON
  escape); and 16 Mi units of fuel, one per iteration of every loop plus one
  per byte scanned, compared or copied and 64 per call into the metric
  source; a lookup in an ordered map charges its key's bytes once per
  level of the map, and one in a hash map twice. Each metric is read from the source once per section expansion.
  `distinct_gt` counts tuples by a 128-bit hash, so two tuples whose hashes
  collide would count as one. On a 9.6-hour recording with a warm reader,
  the slowest construction found takes 0.34 s to reach the fuel limit, about
  22 s for a document of 64 such sections; a cold archive's first read of a
  metric is outside the fuel count. A `TemplateError` has a kind: an invalid
  document, a limit, or an unset parameter.
  `dashboard::rezolus_template::json()` is the Rezolus agent's template, one
  file per section under `crates/dashboard/sections/rezolus/`, about 106 KB
  compact. All sixteen sections are converted; a test renders each from the
  template and from its generator on thirteen metric catalogs, and on a
  catalog of every named metric with each removed in turn, and requires
  identical output. Nothing renders from the template yet.

- `filesystem` sampler: tmpfs usage per mount. Every visible tmpfs mount is
  read on the same sweep into `filesystem_tmpfs_used`,
  `filesystem_tmpfs_size`, `filesystem_tmpfs_inodes_used` and
  `filesystem_tmpfs_inodes_total`, labeled `mount`, `fstype` and `devnum`, so
  a recording shows which of `/tmp`, `/dev/shm`, `/run` or another mount holds
  tmpfs files. Used bytes include pages swapped out; `memory_shmem` counts
  only the resident ones, and also holds SysV shared memory, shared anonymous
  mappings, memfd files and GPU driver buffers, which no mount shows. They are
  separate from
  `filesystem_total` and its siblings, which keep their local-disk meaning. At
  most 64 tmpfs mounts are sampled, apart from the 256 local filesystems;
  over the cap the shallowest paths are placed first. A tmpfs with no size
  limit has its byte series absent. ramfs is not read. The Memory dashboard's
  Page Cache subgroup plots tmpfs use by mount.

- `tcp_mib` sampler: TCP counters the kernel keeps as SNMP MIBs, from the
  `TcpExt` section of `/proc/net/netstat` and the `Tcp` section of
  `/proc/net/snmp`. Delayed-ACK timer firings and deferrals, and duplicate
  data received; RTO expirations, tail loss probes and their recoveries,
  lost-segment retransmits by fast recovery or slow start, spurious RTOs,
  fast recoveries by SACK or Reno, retransmits lost again, D-SACKs received
  and sent by kind, and `tcp_segments_retransmitted`; zero-window transitions,
  withheld zero windows and zero-window drops; receive-queue prunes, prune
  failures, out-of-order prunes, collapsed buffers, backlog drops,
  receive-buffer drops, out-of-order queueing and TCP memory-pressure
  entries; autocorking; listen overflows and drops; aborts by reason; open
  failures, established resets, resets sent, input and checksum errors; and
  `tcp_established_connections` as a gauge. Columns are matched by name, so
  a column the kernel does not print leaves its metric absent. Enabled by
  default, like `memory_vmstat`. The viewer's Network section plots each
  metric under TCP when the recording has it.

- `drivehealth` reads the EC2 NVMe detailed performance statistics (log page
  0xD0) from EBS and instance store volumes on Nitro instances:
  `drive_performance_exceeded_time` (time demand exceeded the volume's or
  instance's IOPS or throughput limit) and `drive_queue_length`. (#1455)

- **Block IO and NVMe drive health on macOS.** `blockio` publishes
  `blockio_operations` and `blockio_bytes` from the storage drivers' IOKit
  statistics, plus the macOS-only `blockio_retries`, `blockio_service_time`
  and `blockio_driver_errors`. `drivehealth` publishes `drive_temperature`,
  the warning and critical temperature-time counters and the thermal-throttle
  counters from each NVMe drive's SMART log. Neither needs root.
- **The viewer follows an archive file that is still being written.**
  `rezolus view` on a `.dendro` that is not finalized, such as
  a running hindsight buffer or a `rezolus record` in progress, reopens it
  and the page refreshes the current section every 5 seconds as in live
  mode. Before, the view showed the file as it was when the viewer opened
  it. On each reopen of a `.dendro` the viewer reads its writer heartbeat:
  while a writer is running it reopens every 2 seconds however rarely rows
  arrive. When no writer is running (heartbeat unchanged for three
  intervals: a killed or paused writer, a `rezolus recording snapshot`
  copy, or a `cp` of a running archive), it reopens every 2 seconds while
  rows arrive and doubles the
  wait after a reopen that finds no new row, up to 60 seconds; a writer
  that beats again returns it to 2 seconds. Following stops when the writer
  finalizes the file, when the file is removed, or when another file is
  loaded. The page keeps its file-mode behavior (file name,
  `from`/`to` in a link, A/B compare); `/api/v1/mode` reports `following`
  rather than `live`. A zoomed window whose start retention evicted is cut
  to the new start, or reset to the full range when none of it is left. In
  compare mode the experiment's charts are fetched again when its range
  changes. A finalized file is not reopened, and an archive whose recordings
  share a label set is not followed. `--tui` redraws a followed file each
  second.
- In live mode and when following a file, the full range that chart
  drill-down zooms within now advances with the recording; before, it was
  fixed at the extent when the first zoom happened.
- `memory_pagecache` sampler (opt-in): the page cache's traffic per mount,
  and per cgroup with `cgroup_attribution = true`. Buffered read calls and
  bytes from one `fentry` on `filemap_read`, pages filled by the filling
  task's context (read, write, fault, other) from the add tracepoint, pages
  evicted, and mmap faults. Pages filled during reads over bytes read is the
  read miss ratio. The memory dashboard's Page Cache group gains the rates
  and the miss ratio when a recording has them.
- `xfs_log` sampler (opt-in): how long threads block on the XFS log, per
  mount and per cgroup, with host-wide latency histograms. Waiting for log
  space is the `xfs_log_grant_sleep`/`_wake` pair; a log force is
  `fentry`/`fexit` on `xfs_log_force` and `xfs_log_force_seq` (the fsync's
  log write); CIL-full waits are counted. The counts equal
  `xfs_log_space_sleeps` and `xfs_log_forces` from `xfs_stats` per mount;
  the time and the cgroup are what the stats file cannot carry. Needs
  kernels 5.12+ with XFS's BTF. The XFS dashboard gains a Blocked Time
  group and the cgroups dashboard an XFS log blocked-time plot.
- `xfs_stats` sampler: XFS's own per-mount counters from
  `/sys/fs/xfs/<dev>/stats/stats`, one sysfs read per XFS mount off the
  scrape cycle (1 s default): log writes, forces and force sleeps, in-core
  log buffer stalls, log-space requests and sleeps, AIL pusher outcomes,
  transactions, inode-cache lookups by outcome and reclaims, extents and
  blocks allocated and freed, directory operations, file calls and bytes,
  and the metadata buffer cache. Labeled `mount`, `fstype`, `devnum` and
  `block_device` through the same slot registry as the ext4 samplers, which
  now also assigns slots to XFS mounts. The viewer gains an XFS section.

- `ext4_ops` sampler (opt-in, never enabled by `[defaults]`): how long fsync,
  unlink, write and rename held the calling thread inside ext4. `ext4_op_latency{op}`
  histograms; per-filesystem `ext4_ops{op}`, `ext4_op_time{op}`,
  `ext4_op_errors{op}` and `ext4_write_bytes`; per-cgroup `cgroup_ext4_ops{op}`
  and `cgroup_ext4_op_time{op}`, the time a service's request threads are held
  inside the filesystem. fsync and unlink from their tracepoints, write and
  rename from `fentry`/`fexit` on `ext4_file_write_iter` and `ext4_rename2`
  (its arity confirmed from BTF), start timestamps in task local storage;
  kernels 5.12+. The ext4 dashboard gains an Operations group and a
  Write Path group that puts application bytes, writeback bytes, journal
  bytes and device bytes on one axis; the cgroup dashboards gain ext4 Blocked
  Time.

- `ext4_journal` and `ext4_alloc` counters are per filesystem: every counter
  carries `mount`, `fstype`, `devnum` and `block_device`, the labels the
  `filesystem` sampler gives the same mount, with `mount="other"` for a
  device the agent's mount table does not know yet. The BPF programs look the
  device up in a `dev_t → slot` map the agent keeps in step with
  `/proc/self/mountinfo` (rescanned every 10 s, and sooner when `other`
  moves); counter banks are per CPU and per slot, 8 MiB and 12 MiB of
  eagerly allocated map for the two samplers. jbd2 events from an ocfs2
  mount get their own slot rather than being folded into the ext4 totals.
  Histograms stay host-wide. The ext4 dashboard draws one line per mount.

- `memory_slabinfo` sampler: sizes of the dentry, VFS/ext4/XFS inode,
  ext4 extent-status, jbd2 journal-head, buffer-head and page-cache-index
  slab caches from `/proc/slabinfo` (`memory_slab_cache_objects{cache,state}`,
  `memory_slab_cache_bytes{cache}`), read at most once per `interval`
  (60 s default) off the scrape cycle. Answers whether the metadata caches
  fit; `ext4_inode_cache` here is the cause behind `ext4_inode_loads`.
  Caches the kernel lacks are absent. The Memory dashboard's Kernel subgroup
  shows the caches when the recording has them.

- `filesystem` sampler: for each ext4 filesystem, `filesystem_errors` (the
  superblock's persisted error count, from sysfs `errors_count`) and
  `filesystem_written_bytes` (lifetime bytes written to the block device,
  journal included, from sysfs `lifetime_write_kbytes`), read on the existing
  60 s sweep and absent on other filesystem types. The Filesystem dashboard
  gains Device Writes and Errors cards.

- `syscall_counts` / `syscall_latency`: a `sync` class (`op="sync"`) for
  `fsync`, `fdatasync`, `sync`, `syncfs` and `msync`, split out of the
  `filesystem` and `memory` classes so their latency, a device round trip,
  is no longer averaged into metadata calls that complete from cache. New
  `syscall{op="sync"}`, `cgroup_syscall{op="sync"}` and
  `syscall_latency{op="sync"}` series; the Syscall dashboard gains a Sync
  subgroup.

- `ext4_alloc` sampler: BPF on ext4's block allocator and the metadata reads
  around it. Per extent allocation, requested versus returned blocks, groups
  scanned and the criterion reached (`ext4_allocations`,
  `ext4_allocation_blocks{kind}`, `ext4_allocation_groups_scanned`,
  `ext4_allocations_by_criterion{criterion}`, `ext4_allocation_size`);
  blocks freed; inodes allocated and freed; writeback passes with pages
  written, skipped and errors; discarded blocks and preallocation releases;
  and the synchronous inode-table and bitmap reads (`ext4_inode_loads`,
  `ext4_bitmap_loads{kind}`). Host-wide. The viewer's ext4 section gains
  Allocator, Inodes, Writeback and Metadata Reads groups.

- `memory_writeback` sampler: BPF on the page-cache writeback tracepoints.
  `writeback_throttle_latency`, `writeback_throttle_checks`,
  `writeback_throttle_events` and `writeback_throttled_time` from
  `balance_dirty_pages` (the sleep the dirty-page throttle imposes on
  writers); `writeback_runs{reason}` from `writeback_start`;
  `writeback_pages_written`. `balance_dirty_pages` has had two argument
  lists, so the sampler reads the tracepoint's argument count from BTF
  (`kernel_btf_tracepoint_arg_count`) and loads the matching program, or
  reports the throttle hook unsupported when neither matches. The viewer's
  Memory section shows throttle latency, throttle rate, runs by reason and
  pages written when the recording has them.

- `memory_vmstat` exports `memory_pages_dirtied` and `memory_pages_written`
  (`nr_dirtied`, `nr_written`): the complete page-cache writeback counts,
  including pages written by integrity syncs, which the flusher's
  `writeback_pages_written` tracepoint does not account for.

- `memory_vmstat` exports the rest of what `/proc/vmstat` says about memory
  pressure, from the file it already reads: the dirty limits the writeback
  throttle uses (`memory_dirty_threshold{kind}`, gauges in bytes); reclaim
  scanned and reclaimed by kswapd versus direct (`memory_reclaim_scanned`,
  `memory_reclaim_reclaimed`), `memory_allocation_stalls` summed over
  zones; `memory_page_faults` and `memory_major_page_faults`; swap in and
  out; working-set refaults, activations and restores by file/anon (5.9+);
  `memory_oom_kills`; THP faults by outcome, collapses and splits;
  compaction stalls and outcomes; and the NUMA balancer's PTE updates, hint
  faults and migrations. Absent lines leave their metric absent. The viewer's
  Memory section gains Reclaim, Working Set and dirty-limit plots, gated on
  the recording.

- `memory_meminfo` exports 30 more gauges from the `/proc/meminfo` it
  already reads: `memory_dirty` and `memory_writeback`; the LRU lists
  (`memory_active`/`memory_inactive` by `kind`, `memory_unevictable`,
  `memory_mlocked`, `memory_shmem`, `memory_mapped`, `memory_anon`); kernel
  memory (`memory_slab` by `kind`, `memory_kernel_reclaimable`,
  `memory_kernel_stack`, `memory_page_tables`, `memory_percpu`); swap
  (`memory_swap_total`/`_free`/`_cached`); overcommit (`memory_commit_limit`,
  `memory_committed`); huge pages (`memory_hugepages_anon`/`_shmem`/`_file`,
  `memory_hugetlb`, `memory_hugetlb_pages` by `state`); and
  `memory_hardware_corrupted`. A line the kernel does not print leaves its
  gauge absent rather than 0. The viewer's Memory section gains Writeback,
  Page Cache, Anonymous, Kernel, Swap and Huge Pages subgroups, shown when
  the recording has the metrics.

### Changed

- **`record` and `hindsight` write dendro archives** (`.dendro`),
  through metriken-archive's `ArchiveWriter`. A bare `rezolus record` writes
  `rezolus.dendro`; hindsight's default output is
  `/var/lib/rezolus/rezolus.dendro`. Groups with slots (threads, cgroups, CPUs) are
  stored one row per occupant, with the occupants' labels in a
  `<table>/occupants` stream. Replayed against the `.rez` writer on four
  recordings, the archives were 3.3–8.8 times smaller, with the same answers
  to 213 of 213 queries on each and a tick p99 of 16–38 ms against
  375–407 ms. Segments are zstd-3 (55–57% smaller than lz4 for about 4% more
  encode time). Built in stages: `record` (#1326), hindsight, whose snapshots are fully sealed
  copies (#1329), `record --stream` (#1336), and `recording` metadata,
  annotate, check, snapshot, filter and combine (#1339, #1340, #1342) and
  Save-as-Report in both viewers (#1345) on dendro archives. Design and
  measurements: `docs/journal/2026-09-28-dendro-writer-adoption.md`.

- The archive reader moved to metriken-archive 0.1.0 (`ArchiveReader`), with
  no change in behaviour: every reader test passes unchanged. `RezReader`
  is now a wrapper that derefs to `ArchiveReader`, so call sites are
  unchanged. `RezDb` implements `metriken_archive::Catalog`, and the
  identity index is read through `reader::IdentityIndex`, an
  `metriken_archive::IndexRelabel`. `catalog::Container::of_path` returns
  `None` for a file that is not a catalog container.

- The WAL row format (`WalGroupRow`, `WalCell`, `WalValue`) and WAL-tail
  materialization moved from `crates/rez/src/wal.rs` to metriken-segment
  0.1.2, and `wal_group_row`/`group_approx_bytes` to metriken-exposition
  0.21.1, with no change in behaviour; `rez::wal` and `rez::rez` re-export
  them. `WalValue::of` is now `rez::wal::wal_value`. The unused pre-dendro
  stream framing in `crates/rez/src/wire.rs` (`StreamFrame`,
  `encode_frame`, `decode_frame`, `encode_frame_filtered`,
  `STREAM_CONTENT_TYPE`) is deleted: `/metrics/stream` has sent dendro's
  replication frames since it shipped.

- The wide segment format, meaning the table model, its parquet encoding
  and decoding, `TableBuilder`/`GroupTableBuilder`, `Window` and the
  `GroupSchema` mirror, moved from `crates/rez` to metriken-segment 0.1.1,
  with no change in behaviour. `crates/rez` re-exports it under the old
  names. `TableBuilder::push_entries` is now the `rez::rez::PushEntries`
  trait. Dependencies: metriken-exposition 0.21.0, metriken-query 0.33.0.

### Added

- `ext4_journal` sampler: BPF on the jbd2 and ext4 tracepoints. Every phase
  of each journal commit (`ext4_journal_commit_latency{phase}`), commit,
  handle and block counts, checkpoint latency and counts, lock-buffer stalls,
  fsync/fdatasync counts and errors (`ext4_sync_file`,
  `ext4_sync_file_errors`), and `ext4_errors`/`ext4_shutdowns`. Host-wide.
  jbd2 reports its phases in jiffies; the sampler converts with a tick
  measured by `clock_getres(CLOCK_MONOTONIC_COARSE)`. The `tp_btf`/`raw_tp`
  twin is chosen per hook by `kernel_btf_has_tracepoints`, which consults
  module BTF as well as vmlinux, since ext4 is a module on some kernels.
  Viewer gains an ext4 section. Design: `docs/journal/2026-09-28-ext4-sampler.md`.

- The reader reads long tables in dendro archives, the 6.0 layout: one row
  per tick and occupant, with each occupant's labels in a parquet stream
  beside the table (`<table>/occupants`; the format is metriken-segment
  0.1.0's, the relabel metriken-query 0.32.0's).
  Series carry the occupant's labels plus an internal `__occupant__`, and
  filters on those labels work. Checked against the same data written one
  column per slot: every query agrees.

- `rezolus view`, `rezolus mcp` and the static-site viewer open dendro
  archives. Checked on two real recordings (581 MB and 1.28 GB): the same
  `mcp query` answers from a `.rez` and its `recording upgrade` conversion,
  per-thread and per-cgroup queries included.

- `recording upgrade --to dendro in.rez -o out.dendro` writes a copy of a
  `.rez` (v1, v2 or v3) as a dendro archive, the container #1224 plans for
  6.0. Recordings become sources and tables become streams; segment, WAL and
  caller-row bytes are copied unchanged, and WAL rows a sealed segment
  already holds are dropped. A timestamp above `i64::MAX` is refused rather
  than wrapped. `-o` must not exist. Measured on a 1.28 GB, 5,874-segment archive: 7.0 s, and the
  output matches the input table by table.

### Changed

- Agent: `syscall{op}` is now per-CPU, one series per op per CPU with an
  `id` label, as `cpu_usage` is (docs/principles.md principle 9). The BPF
  map already kept a bank per CPU; the sampler summed them before export.
  Dashboards sum over CPUs and are unchanged. A Prometheus scrape of the
  exporter now gets per-CPU series, so a query that read the host total
  needs `sum without (id)`. Measured on a 32-CPU host: refresh p50 43 µs and
  p90 96 µs, against 45 µs and 85 µs before.

- dendro 0.2.2 → 0.3.0: `Frame` is `#[non_exhaustive]`, so the recorder's
  frame naming has a catch-all arm. Behaviour is unchanged: the recorder still
  refuses any frame kind it was not written for.

- Hindsight logs the address its HTTP endpoint actually bound, not the
  configured one, so `listen = "127.0.0.1:0"` reports the port it got.

### Fixed

- The A/B quantile heatmap diff pairs each column with the nearest
  window of the other capture, and says how far apart the paired windows
  sit when that is more than 1% of a window. Strided windows fall at
  multiples of the stride, so two recordings' windows sit at different
  phases, and the diff showed the two side by side for about half of
  capture pairs. Steps that differ by under 10 µs count as equal: a
  sub-second stride's stamps carry the error of seconds since the epoch as
  f64, which a 1 ns tolerance refused.

- A compare-mode capture's histogram stride is floored at its own
  interval, where it was floored at the baseline's: an experiment read
  every 10 s against a 1 s baseline took a 5 s Step override as a stride
  shorter than its own rows.

- A Save as Report trimmed to charts that include a histogram's Rate chart
  (`sum(histogram_irate(..))`: runqueue, off-CPU and running time, TCP
  packet latency, Block IO queue wait) but none of its percentile charts
  dropped that histogram's column, and the saved Rate chart was empty:
  metriken-query before 0.37.1 could not read the metrics of that query.

- The valkey template's five KPIs never drew: PromQL read
  `redis/total_commands_processed` as a division. They name their metrics
  as `{__name__="redis/total_commands_processed", source="valkey"}`, which
  metriken-query reads from 0.37.1, and the viewer's instance picker adds
  its matcher to that form.

- The inference-library category section's histogram KPIs (time to first
  token and the other latency charts) never drew: the section wrapped the
  query in `histogram_quantiles(..)` and the viewer wrapped it again, which
  does not parse. The section sends the raw query, as a service section
  does.

- The TUI names a chart's series as the browser viewer does: by the labels
  that differ between them, a device as `<vendor> <id>`, and a series with
  no such label as `Series <n>`. A recording's `host` or `source` no
  longer stands in for the CPU or operation that tells two series apart.

- `memory_writeback` reports unsupported, naming writeback throttling, when
  BTF gives `balance_dirty_pages` an argument count it has no program for;
  it read healthy before. (#1468)

- `rezolus status` and the startup summary give each unsupported hook's
  reason instead of "(no kernel support)". (#1468)

- Sampler init reads kernel BTF once instead of on every query, which took
  about 1.1 to 1.4 s of agent startup on Debian 12 and 13. (#1466)

- `xfs_log`, `memory_pagecache` and `ext4_ops` report unsupported, naming each
  hook left out, where the kernel cannot run it; they read healthy before.
  (#1465)

- Samplers with fentry programs use their kprobe twins, or turn the hook off,
  where fentry cannot attach: aarch64 before 6.4, and x86_64 before 6.6 for
  functions of more than 6 arguments. (#1463)

- `cpu_l3` reports unsupported on a machine without L3 cache perf counters,
  such as a VM without a PMU, so `rezolus status` no longer exits 1 there;
  any other open error is still a failure, now with its errno. (#1457)

- Corrected the swapped `memory_numa_miss` and `memory_numa_foreign`
  descriptions; the NUMA dashboard's remote plot now uses `memory_numa_other`.
  (#1458)

- **`xfs_log` starts when xfs loads.** On a host whose root filesystem is
  not XFS, xfs is a module that loads at the first XFS mount, often after
  the agent has started; `xfs_log` reported unsupported ("the kernel's BTF
  does not describe the XFS log grant tracepoints") and was never retried.
  It now waits for the module as the ext4 samplers do. `xfs_stats` reads
  sysfs on every sweep and already picked up a later mount.
- **ext4 samplers start when ext4 loads.** ext4 and jbd2 are modules on most
  distribution kernels and load at the first ext4 mount, which on a host
  whose root filesystem is not ext4 (Amazon Linux 2023, RHEL) often comes
  after the agent has started at boot. `ext4_alloc` and `ext4_journal` then
  failed to load with "Invalid argument (os error 22)", and `ext4_ops`
  reported unsupported, and none was retried: the host recorded no ext4
  metrics until the agent was restarted. A sampler whose module is a
  loadable module not yet loaded now reports unsupported with "waiting for
  kernel module ext4 to load", checks every 5 s, and initialises once it
  loads. A module the kernel does not have at all is reported unsupported
  at once ("this kernel has no ext4 module").
- In live mode, Record (a reset to a new recording) drops a refresh still in
  flight. A refresh begun before the reset could land after it and draw the
  old recording's data until the next refresh (within 5 s). The reset also
  clears the cached metadata, so a section opened before the next refresh
  queries the new recording's time range.
- The time bar renders, hidden, when the recording has no time span yet. It
  rendered nothing, and its setup then threw, which stopped the page from
  drawing its charts; a live Record whose first redraw saw a single sample
  left the section without charts until the user navigated.
- An aborted query request rejects. It stayed pending, so a refresh or zoom
  whose requests were aborted never finished.
- Save as Report's "Trim to the time range shown" keeps only that range. The
  option sent a range that neither the server nor the browser build read, so
  the report held the whole recording; and it was offered only for a chart zoom, not for a zoom that
  refetched the charts, so a zoomed view usually did not offer it. A report
  keeps whole segments, so it reaches up to one segment's span past each end (5
  minutes in a recorded archive), and a further 5 minutes before the start
  when the archive has long tables, so their occupants stay named. A 1-hour
  trim of a 9.6-hour recording was 154 MB, against 1.27 GB of segments in the
  source. Compare mode does not offer it.
- The viewer's time bar shows the window the charts are drawn for after a
  zoom on a chart, and its Reset button returns to the whole recording.
  Dragging the bar sets that window when the mouse is released, and typing
  a time sets it on Enter; either refetches the charts for it. Before, the bar stayed at the
  whole recording after a chart zoom, and a drag on it zoomed the
  already-zoomed charts by the bar's percentages.
- **Hindsight exits on SIGTERM and SIGINT.** Hindsight treated SIGTERM and
  SIGINT like SIGHUP: it captured the buffer and kept recording. `systemctl
  stop` and `restart` therefore waited out `TimeoutStopSec` (120 s) and
  SIGKILLed the daemon, leaving its buffer directory behind. SIGTERM and
  SIGINT now capture the buffer and exit with status 0, or 1 if that capture
  failed. A stop during a SIGHUP capture exits when that capture completes;
  a second stop exits at once with status 2 and removes the buffer
  directory. SIGHUP captures and keeps recording, and a SIGHUP during a
  capture is ignored.
- A dendro archive's metric that first appears in a later segment of its
  table, or only in the live tail, can be queried; the reader learned a
  table's metric names from its first segment alone (metriken-archive
  0.2.8). A dendro archive's segments now also carry a format version
  (`metriken.format`), and the reader refuses a newer format, an unknown
  layout, or a source written by an encoder it does not decode, instead of
  misreading it (metriken-segment 0.1.5, metriken-query 0.33.2).

- Agent: a `/metrics/stream` interval with no reading to send (before the
  first sampling pass, or when a snapshot failed to encode) now gets an empty
  `Rows` frame with the next `seq`, as every other interval does. It was
  skipped, which left a gap a subscriber reads as lost intervals, and a
  subscriber waiting on its first interval waited for the next one.

- Agent: `cpu_usage` lost CPU from the per-CPU and per-cgroup totals under
  task churn, not only from the per-task view. Two causes, both in how a new
  task is started in BPF:
  - A task whose `task_info` event did not fit in the ring buffer (drained
    only when a snapshot is taken, about 1,130 events) looked new on every
    accounting hit and had its baseline re-zeroed each time, so every delta
    was skipped. The baseline is now set once per task instance and only the
    metadata send is retried.
  - Every task's first observation was skipped, which drops all the CPU of a
    thread that lives about one tick, and each field's first non-zero value.
    A task that started after the agent attached is now counted from zero; one
    that predates it is counted from its first observation, so an agent
    restart still does not credit running tasks' lifetime CPU to one tick.

  Measured on a 32-core host with 60 s of `stress-ng --pthread` (about 18,000
  threads/s) against the kernel's `cpu.stat`: the cgroup's CPU went from 22%
  of the kernel's figure (5.20.0) to 85%, system time from 20 to 80 of 94
  core-seconds. Without churn, totals were already within 1%.

- Recorder: retention of the identity index kept too little. It cut each
  stream's `caller_rows` at the latest `Full` at or before the row cutoff, but
  a segment is evicted only when its newest row is older than the cutoff, so a
  segment spanning the cutoff kept rows older than it and lost the `Full`
  they depend on. The reader would then skip the `Delta`s before its first
  `Full`, so up to one seal age (300 s) of rows at the old end of a rolling
  buffer would read without their task and cgroup labels. The cut is now the
  latest `Full` at or before the oldest row the stream still holds after
  eviction. No shipped path hit this: `hindsight` is the only caller of
  retention and it scrapes, so it writes no index, and `record --stream`
  writes the index but never evicts. It would have appeared once the rolling
  buffer took the stream. (introduced in #1281, v5.22.0)

## [5.22.1] - 2026-09-24

### Changed

- Viewer/dashboard: the section payload no longer carries `num_series`.
  Computing it counted every label set of every metric, which on a `.rez`
  forced every table's reader to be built — for an indexed table, replaying
  the identity index — the first time `/api/v1/sections` was asked, and
  re-walked the labels on every ask after that, undoing the lazy open for
  the first request a page makes. The rezolus viewer never rendered the
  number; systemslab's metrics page showed it as "N series" and drops that
  label in iopsystems/systemslab#6268.

## [5.22.0] - 2026-09-23

### Added

- Reader: a group table whose slots the identity index describes is read
  through it. A group table's columns are its slots, and a slot changes hands
  — a task exits and another lands in its BPF map slot, a cgroup id is
  recycled — so a column's own labels can only say what the slot meant when
  the segment was written. The reader now replays the stream's `caller_rows`
  into per-slot occupancy spans and cuts each column into one series per
  occupant, labelled with what the index says, with the occupant's `__uid__`
  on it. On archives that carry identity in both places today the two read
  paths agree series for series; once the writer stops copying identity into
  column metadata, the index is where the labels live. (#1280, #1284)
- Recorder: `record --stream` restates every stream's slot set into the
  archive every 300 s (the seal policy's age bound), so a rolling buffer can
  evict identity history and a reader of a long recording replays one
  restatement period rather than everything since connect. Retention cuts a
  stream's `caller_rows` back to the latest full restatement at or before the
  row cutoff and never past it; a stream with none on record keeps its
  history. (#1281)

### Changed

- Reader: a `.rez` table's segments are fetched from the archive as a query
  touches them, through a cache bounded by `--cache-size-mb`, instead of
  every segment's bytes and parsed footer being held for the reader's life.
  Open reads each footer once for the identity indexes and a per-segment time
  span, then lets it go. On a 1.3 GB, 9.6-hour archive whose per-task table
  had 159 segments of up to 2,851 columns, the viewer's resident size after
  building its dashboard went from 4.1 GB to 0.25 GB and `mcp
  describe-metrics` from 4.4 GB to 0.25 GB. Needs metriken-query 0.27.0.
  (#1283)
- Reader: `rate()` and `irate()` read each series as a sample stream and keep
  one interval's worth of it, so a query over many series costs the series it
  is looking at rather than the whole table. On the same archive,
  `sum(rate(task_cpu_usage[1m]))` over 6,644 task series peaked at 11.9 GB
  before and 0.98 GB after, with identical output. Needs metriken-query
  0.29.0. (#1286)
- Reader: composing a recording (systemslab's path) hands out one lazy child
  per table, built from the catalog the reader probes at open; a table opens
  the first time a composed query names one of its metrics, and composed
  `rate()` queries stream. Composing this archive used to open every table's
  every segment footer before any query. Needs metriken-query 0.30.0.
  (#1287)

### Fixed

- Reader: replaying a stream's identity index diffed the whole live slot set
  after every entry and serialized every slot for a state hash the reader
  never reads. On a ten-minute recording under task churn (248,511 entries)
  a query took 20 s; it takes 0.6 s. (#1282)

## [5.21.0] - 2026-09-23

### Added

- Agent: a `cpu_power` sampler reports CPU energy, power and idle-state
  residency, with Power and C-State Residency sections in the CPU dashboard.
  Everything is read through perf: the sampler discovers the `power`,
  `power_core`, `cstate_core` and `cstate_pkg` PMUs from
  `/sys/bus/event_source/devices` and opens counters on the CPUs each PMU's
  cpumask permits, so there is no vendor or model detection — which PMUs exist,
  which events they expose and which CPUs may read them are all properties the
  kernel already publishes, and they vary by part as much as by vendor. (#1071)
- Agent: an Intel GPU sampler, `gpu_intel_pmu`, covering integrated GPUs and
  discrete Arc cards through the i915/xe PMU. Each GPU registers its own PMU,
  discovered at init along with the engine set that GPU actually exposes: the
  perf type is assigned at boot and the engine set differs per part, so nothing
  is hardcoded. The viewer's per-GPU charts distinguish two vendors on one
  host. (#1173)
- Agent: a `filesystem` sampler reports total, free and available bytes and
  total and free inodes, plus whether it is read-only, for every locally mounted
  filesystem: one series per filesystem, labeled by `mount`, `fstype`, `devnum`
  and, for block-backed filesystems, `block_device`. The mount table is re-read on each sweep, so
  a filesystem mounted after startup appears on its own. Local filesystems
  only: network filesystems, FUSE and autofs are never sampled, and neither is a
  local filesystem another mount covers or that sits below a network, FUSE or
  autofs mount, because a lookup or `statvfs` that reaches a
  hard network mount blocks until the server answers. Sweeps
  run at most once per `[samplers.filesystem] interval` (default 60s), off the
  scrape cycle. The viewer gains a Filesystem section with one line per filesystem.
  (#1202)
- Agent: an opt-in Linux `hw_sensors` sampler reads native thermal zones, hwmon
  temperature and electrical channels, fan RPM/PWM, and cooling states. Includes
  INA3221 power derivation and Thor board interpretation, with a Hardware Sensors viewer
  section. Reads run in the blocking pool at a configurable interval (default
  5s). Thor inventory fixtures and synthetic Orin layouts are tested; hardware
  timing and Orin validation are still required before fleet-wide enablement.
  (#1203)
- Agent: `network_host_bytes` and `network_host_packets` count north/south
  traffic once, at the host boundary. The existing `network_bytes`/
  `network_packets` count TX once per `net_device` the skb is handed to rather
  than once per packet leaving the host, so a bonded host reports about twice
  its real egress and VLAN-over-bond three times. RX is not symmetric, which
  makes the older pair asymmetrically inflated rather than uniformly wrong. The
  new counters are additive; the old pair is unchanged. (#1214)
- Viewer: the host-boundary series appears above the per-interface panels in
  both dashboards and the TUI, rather than replacing them. The per-interface
  sum measured 3.04x the real figure on a bonded host, and about 1000x at idle
  on one running VMs, where loopback and virtual-interface traffic dominate.
  (#1219)
- Agent: `/metrics/rows`, a row-format endpoint that names a schema by hash
  instead of resending it on every scrape. `/metrics/binary` ships every
  acquisition group's full `GroupSchema` every time because each consumer
  decodes one snapshot in isolation; a consumer that keeps a cache does not
  need that. (#1237)
- Agent: a sampling clock of its own, running only while something is
  subscribed. `refresh()` was previously reachable from one place — a request
  whose TTL had expired — so the request was the cadence. Demand-driven rather
  than unconditional because a cross-CPU perf read measured 1002us p50 idle
  against 52us under load (19.3x): a perf event opened on another CPU charges
  that core's C-state exit latency to the read. (#1240)
- Agent: a sampling pass is served as dendro replication frames on
  `/metrics/stream`, with a slot index so that per-CPU churn stops resending
  schemas, and the index entries for a tick commit in the same transaction as
  the rows they describe. (#1247, #1249, #1254, #1257, #1260, #1266, #1267)
- Recorder: `--stream` subscribes to an agent's `/metrics/stream` instead of
  scraping it. A pump task per endpoint holds the subscription and the tick
  loop drains it, staging rows through the same path the row endpoint feeds.
  The recording is anchored on the handshake's clock and epoch. (#1261, #1272)
- Agent and recorder: a body carries the moment the values were read, and a
  recording keeps it. A timestamp used to be whatever clock read last — the
  recorder's tick when scraping, frame-emission time when streaming — and
  neither names the sample. Because the agent serves a cached pass for its TTL,
  a scrape inside that window gets values read up to a TTL earlier and one HTTP
  response looks the same either way. (#1269, #1270)
- Recorder: a capture records the version of the agent that produced it, under
  the `version` metadata key, in the `.rez` per-recording manifest and the
  parquet footer alike. Sampler behavior changes between builds, so when two
  recordings of one host disagree the first question is which agent versions
  wrote them — previously unanswerable from the files. (#1211)
- Agent: a `producer_epoch`, a v4 UUID minted once per process and carried on
  every snapshot. A consumer cannot otherwise tell a counter that was RESET
  from one that WRAPPED — both went down, and the arithmetic differs. The epoch
  changes when, and only when, every cumulative counter restarted from zero
  together. (#1231)
- Agent: a slot assignment mints its occupant's `__uid__`. A slot's labels say
  what it means, and two different things can carry the same labels — a PID
  wraps onto the same comm, a cgroup is deleted and recreated at the same path,
  a task restarts under the same name. A reader keying series on labels alone
  fuses those into one series with a reset in the middle, and attributes one
  task's counter to another. The uid is minted once per assignment from the
  assignment's generation and the producer epoch, and travels with the labels
  everywhere they already go: snapshot descriptors, `.rez` column metadata, and
  the identity index a stream carries. (#1276)
- Site: a physical AI section and a homepage theme toggle (#1262), and the
  README is now a guide to Rezolus workflows (#1263).

### Changed

- Agent: `network_traffic` attaches BTF-typed (`tp_btf`) rather than `raw_tp`.
  Without argument types the verifier compiles every field access to a
  `bpf_probe_read_kernel` call, and on a probe whose whole body is ~45ns one or
  two of those dominate it — which is why the host-boundary counters in #1214
  cost +28.8ns/run (+64%). (#1235)
- Agent: the schema cache validates by metadata version rather than by hashing
  labels. It folded every entry's label map into a 128-bit hash, for every slot
  of every group, every tick, so that a slot relabelled at a stable index would
  be noticed — measured at 13-19% of the agent's sampling CPU on a 32-core host
  in every build profiled. Labels only change on a slot assignment, and the
  metric store bumps a version on every mutation, so one version per metric now
  stands in for the fold. (#1277)
- Recorder: provenance describes a recording rather than a metric, and the
  Prometheus converter no longer labels individual metrics with it. (#1258,
  #1259)
- Consumers hide internal labels by rule rather than by name. A `__`-prefixed
  label is identity-bearing and matchable but dropped by `without` and hidden
  in output, so every place that hid `__name__` by name, or listed storage keys
  by name, now goes through one predicate — `is_internal_label`/`is_storage_key`
  in the metric catalog, MCP `describe-metrics`, anomaly and correlation
  selectors and the TUI legend, and a shared `labels.js` for the web legends,
  boxplot, compare and explorers. Without this a `__`-prefixed label would have
  become the legend text of every series carrying it, because the frontend
  named a series by the first label that is not `__name__` and labels arrive
  sorted. (#1275)

### Fixed

- Viewer: a recording made below a one-second cadence is read and drawn at that
  cadence instead of at one second. The frontend floored every query step and
  histogram stride at 1s, so nine of every ten samples of a `--interval 100ms`
  recording were discarded and a 2 Hz signal came back as a flat line at its
  mean. Nothing in the query path required the floor — the engine takes `step`
  as seconds and works in nanoseconds — and the Step selector now offers the
  sub-second choices such a recording can resolve. (#1273)
- `.rez`: a sub-second archive reads back the values it holds. The query engine
  rounded every timestamp to a grid taken from the file's declared
  `sampling_interval_ms` — a key a `.rez` segment does not carry, so it assumed
  one second — and ten rows of a 100ms second collapsed onto one instant,
  leaving a sub-second query returning held-forward copies of each second's
  survivor. metriken-query 0.24.0 reads the recorded timestamps instead.
  **Existing archives were never damaged, only misread**: they need no
  re-recording. `RezReader::interval()` now reports a measured cadence rather
  than the 1s default every `.rez` inherited, which is what lets the viewer
  open a sub-second recording at its own resolution.
- Agent: `network_ethtool` counters have per-interface identity. One
  `&'static LazyCounter` was shared across every interface per stat NAME and
  written with `set()`, so on a host where more than one interface exposes ENA
  allowance stats, whichever interface `read_dir` enumerated last silently
  overwrote the others. Multi-ENI EC2 instances are ordinary, so this was not a
  corner case on the hosts this sampler exists to serve. (#1216)
- Hindsight: the daemon writes its buffer out before exiting. The packaged
  service set `KillSignal=SIGKILL`, which cannot be caught, so every
  `systemctl stop` and `restart` threw away the lookback window — the one thing
  a flight recorder exists to keep — and orphaned the staging directory, so
  repeated restarts accumulated buffers until the filesystem filled. (#1225)
- Hindsight: the rolling buffer gets its own directory under `/var/lib`. It
  previously went wherever `output` pointed, defaulting to `/tmp/rezolus.rez`,
  so out of the box a buffer sized for a useful lookback — hundreds of
  megabytes to gigabytes — was created on tmpfs. That is memory with extra
  steps, competing with the workload being observed, on the same host, during
  the incident being recorded. (#1234)
- Agent: a tap device is present, not absent. `collect_nics` kept only
  interfaces whose `operstate` is exactly `up`, and a tap reports `unknown` for
  its whole life because there is no carrier to report on, so every VM tap and
  tun interface was missing from the hardware inventory embedded in every
  recording taken on a hypervisor or VPN host. (#1236)
- Agent: a per-CPU group declares the CPUs the host has, not the slots it could
  have. Membership was a dense prefix over `possible_cpus()`, which returns
  `max_id + 1` over hot-addable ids and may have gaps. (#1244)
- `.rez`: a relabeled group slot gets its own column rather than the previous
  occupant's. A slot's descriptor name is `{metric_id}x{slot}` whatever its
  labels are, and columns were keyed on that name alone with metadata copied
  only at creation, so a reused slot wrote into the first identity's column and
  kept its labels. (#1232)
- `.rez`: an index entry's state belongs to the source, not to one stream.
  dendro's subscriber keeps one `index_state` per source, so with several
  groups emitting index frames in a tick only the last one's state survived.
  (#1251)
- Viewer: every per-point column is clipped when serving a display tile.
  `uncLo`, `uncHi` and the interpolated flags are indexed by the same point
  index as `t` but came through at full length, misaligned by the window's
  start. (#1250)
- Viewer: only a decimated tile is judged against the point budget. A tile that
  was never decimated already holds every sample the recording has across its
  range, so no refetch can return more, and rejecting it forced one anyway.
  (#1252)
- Agent: one clock anchor per agent, not one per connection. Every subscription
  minted its own anchor and advertised it in the handshake, so two subscribers
  to one agent were told the same source had two timelines. (#1268)
- Recorder: a scrape's window is read off the row's own clock. `request_ns`/
  `response_ns` came from a raw wall reading while the row holding them is
  stamped on the recorder's anchored timeline, and the archive stores a window
  as an offset from its row's `ts` — so the offset carried the wall-versus-
  anchor divergence instead of the read's position within the tick. (#1271)

### Security

- rustls 0.23.45, for RUSTSEC-2026-0285: TLS 1.3 handshake messages sent at the
  wrong encryption level were accepted when they followed a key-changing
  message in the same record, where RFC 8446 section 5.1 requires terminating
  the connection. (#1212)

### Dependencies

- Two grouped dependency updates, 23 and 21 crates (#1210, #1264). The second
  takes `histogram` 1.6.0, which validates configurations and bucket counts on
  deserialize; the exporter now relies on that rather than re-checking a
  histogram it cannot rebuild.
- dendro moves from git to crates.io and then to 0.2.0 and 0.2.1, pinning the
  replication contract the streaming endpoints are built on. (#1230, #1246,
  #1255)
- metriken-query 0.25.0. 0.24.0 stopped rounding a recording's timestamps to a
  declared sampling grid, which is what lets a sub-second recording read back
  the values it holds; 0.25.0 makes `__`-prefixed labels internal. (#1273,
  #1275)

## [5.20.0] - 2026-09-10

### Added

- Viewer: stretches of time the producer never observed are drawn as a dashed,
  band-less segment rather than in the same ink as measured data. `rate()`
  across a hole has a value but no honest uncertainty bound, and was previously
  indistinguishable from a real point. Applies to both the display and matrix
  paths, and per-capture in A/B compare. (#1193, #1199)
- Viewer: the compare badge lists every capture in an N-way comparison, not
  just the first two. (#1172)

### Changed

- **`cgroup_cpu_bandwidth_quota` and `cgroup_cpu_bandwidth_period` now report
  real values instead of `0`.** The `tg_set_cfs_bandwidth` kprobe read `PARM2`
  as a pointer when the kernel passes a `u64`, which faulted on modern kernels.
  Anything that encoded the zero — a dashboard panel, an alert threshold — will
  see these series move. The sampler also gains fentry twins, with the kprobes
  kept as the CO-RE-only fallback. (#1166, #1168)
- **An unwritten counter-group entry now reads as absent rather than zero**, so
  a sampler that populates part of a group no longer publishes phantom zero
  series for the rest. Externally-backed (BPF mmap) groups are deliberately
  unaffected: that memory is kernel zero-filled and cannot carry a sentinel.
  (#1188, #1190)
- Debian 11 packages are no longer published. The distribution is end-of-life.
  (#1179)
- Dependencies: `metriken` 0.11.0, `metriken-exposition` 0.20.0,
  `metriken-query` 0.23.0, plus 11 routine updates. (#1174, #1188, #1190)

### Fixed

- Viewer: a single hole no longer strips the uncertainty band from an entire
  decimated series. (#1193)

## [5.19.1] - 2026-09-09

### Added

- Agent: block IO latency is now split into its three phases, each a histogram
  broken down by `op` (read, write, flush, discard). `blockio_queue_latency` is
  the time a request waited before the device began servicing it — the
  component that grows under saturation, where device latency alone stays flat.
  `blockio_total_latency` is the end-to-end span the issuing workload actually
  experiences, measured directly rather than summed (two histograms cannot be
  added). The sampler already traced `block_rq_insert`, but insert and issue
  shared one timestamp and issue overwrote it, discarding the interval — so
  this needs no new probe attach. A request that goes straight to the driver
  has no queue phase and records no queue sample. (#1054, #1124)

### Changed

- **Agent: `blockio_latency` is renamed `blockio_device_latency`.** It is the
  same measurement — the pre-split metric always reported the issue-to-complete
  span — but the bare name became ambiguous once the queue and end-to-end
  phases were split out alongside it. Readers treat the two names as one
  metric: the viewer, dashboard, and TUI resolve whichever name a recording
  carries (`blockio_device_latency_metric`), so existing recordings keep
  rendering unchanged. External consumers that query `blockio_latency` by name
  — Prometheus rules, Grafana dashboards, saved PromQL — need updating.
  (#1054, #1124)

### Fixed

- Exporter: `/metrics/binary` served zero bytes when the agent could not be
  reached. To a scraper that only reads the body that is the same as an empty
  snapshot, but not to a consumer that probes the endpoint to decide what it
  speaks: zero bytes are not a msgpack document, so the probe concluded the
  endpoint was Prometheus and fell through to the text route. `rezolus record`
  probes exactly that way, so pointed at an exporter whose source had not come
  up yet it reported `detected Prometheus`, refused with `.rez requires a
  rezolus (msgpack) endpoint`, and recorded nothing for the whole run — even
  when the source appeared seconds later. It now serves a valid msgpack
  document carrying no metrics, which is both true and recoverable: the
  consumer keeps its connection and picks up real data the moment the agent
  appears. An empty body *from* the agent is normalized the same way. (#1176)

### Removed

- **Debian 11 (bullseye) packages are no longer published.** Debian 11 has
  reached end of life: `bullseye-security` was last signed on 2026-08-31 and
  its `Release` file expired on 2026-09-07. apt refuses an expired `Release`,
  so `apt-get update` fails inside the build container and cannot be made to
  succeed without disabling the freshness check — which would mean shipping
  packages built against an archive that receives no further security updates.
  Debian 11 users should stay on 5.19.0 or upgrade to bookworm or trixie. The
  install script and the supported-distribution list are updated to match, so
  it reports the release as unsupported rather than adding a repository that
  has no packages for it.

## [5.19.0] - 2026-08-31

### Changed

- Agent: a sampler for a capability the machine lacks now reports `unsupported`
  rather than `failed`, and `rezolus status` no longer exits non-zero because of
  it. A host whose CPU has no L3 cache domain, or which has no ROCm library or
  ethtool support, used to fail a readiness check over hardware it was never
  going to have. Monitoring that alerts on the status exit code will stop firing
  on such hosts; a genuine initialization failure still exits non-zero, and
  `disabled`, `unsupported` and `failed` are now three answers rather than two.
  (#1102, #1103)
- GPU: a Tegra SoC's integrated GPU no longer records utilization or PCIe link
  metrics. NVML answers both queries there and answers wrongly — a hard-coded
  0% utilization indistinguishable from a genuinely idle GPU once it is a row in
  an archive, and a placeholder generation and width for a link an iGPU does not
  have. `gpu_sm_utilization`, `gpu_dram_bandwidth_utilization` and the PCIe
  series are absent on those parts now rather than present and wrong. (#1108)
- Viewer/MCP: a query against a `.rez` archive is 12.8x faster. A query opens
  only the tables it touches, where before it materialized the whole archive and
  spent 91% of its wall time opening tables it never read. `.rez` is now at or
  ahead of a single parquet holding the same window. No format change — existing
  archives get this on read. (#1107)

### Added

- Recorder: several endpoints record into one `.rez`. `rezolus record --endpoint
  http://web-01:4241 --endpoint http://web-02:4241 -o fleet.rez` writes one
  archive holding a recording per host, where before more than one endpoint
  silently demoted the output to parquet. Both arms of an A/B can now be
  captured concurrently rather than in sequence, so they share their background
  load instead of differing in it as well as in the experiment. (#1109)
- Packaging: `.rpm` packages for Enterprise Linux 10, and the install script now
  accepts Rocky Linux, AlmaLinux, RHEL, CentOS Stream and Oracle Linux at major
  9 or 10. `curl -fsSL https://install.rezolus.com | sudo bash` failed outright
  on EL10 before. (#1113)

### Fixed

- Recording: reading a `.rez` while it is being written no longer panics when
  retention evicts a table mid-query. A quiet sampler's only rows could be
  dropped between the probe that named its table and the query that opened it,
  crashing the viewer or MCP against a hindsight rolling buffer — which is the
  one thing that buffer exists to be read as. A table that has gone is reported
  absent, the same as one that was never there. (#1112)
- Recording: a Prometheus source that emits timestamps no longer writes
  acquisition windows anchored decades before the rows holding them. The
  optional trailing timestamp in the exposition format is a federation staleness
  marker, not a claim about when we read; taking it at face value gave a row
  recorded in 2026 a window one second after the Unix epoch — a ~56-year error
  in the operand `rate()` prices its uncertainty from, nonsense rather than
  conservative. The window is our fetch. Any pushgateway or federation exporter
  produced this, on the parquet path as well as `.rez`. (#1111)

## [5.18.0] - 2026-08-26

### Changed

- Scheduler: runqueue latency no longer counts the idle task, so it measures
  time real tasks spent waiting instead of time CPUs spent asleep. Reported
  latency drops, and the spurious outliers in the top histogram bucket — an
  underflow from a slot shared across CPUs — are gone. Alert thresholds tuned
  against the old numbers will need revisiting. (#1035)
- Scheduler: involuntary context switches no longer count a CPU waking from
  idle as a preemption. On a lightly loaded machine this was most of the count,
  charged to the root cgroup, so systems appeared to be under preemption
  pressure that did not exist. (#1046)
- Scheduler: context switches are now split into voluntary and involuntary.
  Only involuntary was emitted before, which made "no voluntary switches"
  indistinguishable from "not measured" — so a blocking handoff between two
  processes on a pipe looked the same as real preemption, despite pointing at
  completely different causes. (#1040)
- CPU: a task's CPU time is no longer lost when it exits. The counter and its
  labels are now released together, so the series ends cleanly rather than the
  task's total going missing. Workloads with heavy process churn were
  undercounted. (#1037)
- BPF: perf counters that never measured anything are no longer published, so a
  zero can be trusted. A counter that never got a PMU slot used to be
  indistinguishable from one that legitimately measured zero, which is common
  under virtualization where the PMU may not be available at all. Metrics that
  were previously present-and-zero will now be absent on such hosts. (#1052)
- Agent: `snapshot_format` defaults to v3, which reshapes `/metrics/json` from a
  flat `{counters, gauges, histograms}` into `{groups: [...]}`, and changes
  `/metrics/binary` with it. In exchange, each reading carries the window it was
  actually acquired over, making `rate()` error bars precise rather than
  approximated — and scrapes cost roughly half the wall time and a third of the
  agent CPU. Anything parsing those endpoints directly needs updating; set
  `[general] snapshot_format = "v2"` to defer. (#1076)
- Recorder: `rezolus record` writes `rezolus.rez` by default rather than
  `rezolus.parquet`. The extension picks the format now, so `--format` is rarely
  needed. Scripts that record with no arguments and then read `rezolus.parquet`
  will not find it — they fail at the read rather than silently recording the
  wrong thing. A `--format` contradicting the extension is an error rather than
  a silent choice between them. (#1097)
- Recorder: `record --node` and `--instance` are gone. No reader was ever
  written for either, so they accepted a value and dropped it; anything that
  looked like it was labelling a recording was not. `--metadata node=NAME` and
  `--metadata instance=NAME` write the keys `combine` actually reads. Passing
  the old flags is now an error rather than a silent no-op. (#1097)
- Recording: the `parquet` subcommand group is now `recording`, since these
  commands stopped being parquet-specific once `.rez` arrived. `rezolus parquet
  ...` still works, so existing scripts keep running. (#1075)
- Recorder: the tar `.rez` container is no longer written. Archives from older
  releases still open everywhere, and `recording upgrade` converts one. (#1074)
- Status: `rezolus status` with no arguments asks the agent on this machine, and
  accepts a bare host or `host:port`. It exits non-zero when any sampler is
  unhealthy, so it works as a readiness probe or a gate in a script. (#1099)
- Agent: a cached snapshot is served without re-encoding it. Every request used
  to rebuild a multi-megabyte body and throw it away; 500 requests against one
  cached snapshot went from about 2 MB of RSS growth to none. (#1084)

### Added

- Recorder: `.rez` recordings, where each sampler records at its own cadence and
  carries the window its reading actually covered. That window is what lets
  `rate()` and `irate()` report uncertainty bounds instead of a bare number, so
  you can tell a real change from sampling noise. Archives are around a third
  the size of the equivalent parquet and query about twice as fast. A recording
  is also valid at every instant: it is readable while still being written,
  survives a SIGKILL or power loss having lost at most one interval, and stops
  in the same bounded time whether it ran for a minute or a day. (#1017, #1041,
  #1060, #1061, #1070)
- Recording: queries spanning two samplers of a `.rez` are answered instead of
  refused, so ratios like cycles-per-instruction across samplers work. Where the
  two cadences differ materially the query is evaluated at the slow sampler's
  own timestamps, so its values are not silently held forward across points it
  never measured. (#1080, #1082)
- Recording: `convert` turns a raw msgpack recording into parquet after the
  fact, so a long unattended capture can record at the lowest possible overhead
  and pay the conversion cost later. (#1059)
- Recording: `upgrade` brings a v1/v2 (tar) `.rez` onto the current container.
  `combine`, `filter` and `annotate` do it in passing, so older archives keep
  working without a migration step. (#1073, #1074)
- Viewer: `--tui` explores a recording or a live agent from the terminal, for
  looking at a machine over ssh without forwarding a port or opening a browser.
  (#1019)
- MCP: `extract-features` summarizes a recording as a deterministic, versioned
  JSON record, so an agent can assess a capture without reading every series.
  (#1028, #1029, #1030, #1031, #1032)
- Agent: PMU hardware counters are budgeted rather than over-subscribed.
  Excess events used to open successfully, never get scheduled and read nothing,
  with which samplers won decided by an unspecified link order that could change
  between builds — and on a hypervisor this reached past the host, leaving
  guests counting nothing while reporting themselves healthy. Counters are now
  allocated per-PMU by rank, all-or-nothing, and `rezolus status` names the
  samplers that lost as pmu-limited or pmu-starved. `reserved_pmu_counters` and
  `reserved_pmu_cpus` leave headroom for other tooling on the box. (#1093,
  #1096)
- Viewer: rate time-alignment modes (Aligned/Raw) (#1023), sampling-jitter
  visualization for simple-capture parquets (#1014), and a swatch row explaining
  what each shade means on charts with band fills (#1021).

### Fixed

- Agent: a config file that omits the `[general]` table starts with the
  documented defaults instead of exiting with "ttl couldn't be parsed: value was
  empty". (#1099)
- Agent: `cpu_perf` opens its per-CPU events as one perf event group, so its
  counters describe the same slice of time and derived ratios are meaningful.
  (#1091)
- Agent: a host with no GPU no longer ships GPU tables in every recording. It
  used to carry 448 phantom AMD members and 672 NVIDIA ones, all null. (#1081)
- Agent: groups with no live sampler declare no members (#1086), and V2 external
  metrics get their own column key (#1090).
- Recorder: a `.rez` recording that cannot start now exits non-zero. It printed
  an error and exited 0 before, so a `record && analyze` pipeline carried on and
  analyzed whatever a previous run had left at that path. A usage error also
  prints one line instead of a panic backtrace. (#1097)
- Recorder: a malformed histogram cell can no longer wedge a `.rez` recording
  (#1077), and segment size accounting is corrected so seals are not
  mistimed (#1050).
- Exporter: a histogram that cannot be downsampled is dropped rather than
  panicking, so one bad metric no longer takes the exporter down. (#1088)
- CLI: six help-text claims that contradicted the code were corrected — three
  commands said v3 `.rez` archives "error clearly" long after they worked, which
  told users a working operation was unavailable. A test now fails the build if
  a help text names a flag its command does not define, or if an example
  invocation uses one. (#1098)
- CLI: the published documentation was five weeks behind the binary, listing two
  flags that now exit non-zero and omitting `status`, `.rez` and `--tui`
  entirely. (#1100)
- Analysis: subsystems are inferred from metric names, and absences that cannot
  be known are never asserted. (#1034)
- Viewer: restored a missing `boxplot.js` symlink that had taken the deployed
  viewer down, with a CI guard against recurrence. (#1012)
- Installer: the apt repository architecture is detected instead of hardcoded to
  amd64, so arm64 installs work. (#1018)

## [5.17.0] - 2026-07-16

### Added

- Viewer: server-side display decimation with a min/max envelope and zoom
  drill-down. Charts on long recordings render fast from a median + min/max
  envelope so short-lived spikes survive decimation, and zooming refetches
  the window at finer resolution. Applies to both the server and WASM
  (browser) viewers. (#1006)

### Changed

- Recorder: parquet files are written with smaller row groups (1800 rows)
  so the viewer can drill into zoomed windows faster. (#1003)

### Fixed

- Viewer: the WASM viewer package builds again — wasm-pack was invoked with
  a conflicting `--profile` flag. The size optimizations are now applied via
  environment overrides scoped to the WASM build. (#1007)
- Viewer: embedded JS assets are now revalidated with ETags, so browsers no
  longer serve a stale mix of modules after upgrading Rezolus. Client-only
  routes also no longer trigger a spurious section reload and console error.
  (#1010)

## [5.16.2] - 2026-07-09

### Fixed

- Viewer: non-Rezolus "simple capture" parquets now render correctly in the
  browser (WASM) viewer. The per-source `source:` section and its metric
  browser appear instead of empty Rezolus built-in sections. Source
  classification is now shared between the server and WASM backends, and also
  applies to captures uploaded to the server. (#997)
- Viewer: the chart Expand link now works in simple-capture (source) sections.
  Expanding a metric chart reconstructs it from the metric catalog and opens it
  in the single-chart view instead of landing on a blank route. (#998)

## [5.16.1] - 2026-07-08

### Fixed

- Viewer: chart queries now fetch the whole recording at its native
  sampling interval instead of a decimated trailing window, so charts
  render on load and short-lived spikes are no longer averaged away
  before they reach the display. The native sampling interval is now
  exposed by the metadata endpoints (server and WASM). (#995)

## [5.16.0] - 2026-07-07

### Added

- Agent (AMD GPU): new `gpu_amd_smi` sampler collects AMD GPU
  utilization, memory, power, temperature, and clock metrics via ROCm
  SMI, with a matching viewer dashboard. NVIDIA GPU metrics now carry a
  `vendor="nvidia"` label so multi-vendor fleets can be distinguished.
  (#973)
- Agent (AMD GPU): additional `gpu_amd_pmu` sampler reads AMD GPU
  hardware performance counters directly. It has high CPU and memory
  overhead and is therefore disabled by default; enable it explicitly
  and set the desired `perf_level`. (#973)
- Agent (drive health): `drivehealth` reports per-drive temperature via
  SMART pass-through ioctls, alongside the existing health metrics.
  (#992)
- Agent: sampler health classification and a new `rezolus agent status`
  command that reports each sampler as active/disabled/failed. (#977)
- Recorder: perf-record-style command wrapping — `rezolus record` can
  launch and record around a target command, capturing metrics for the
  span of that process. (#981)
- Viewer: "Save as Report" in two-file A/B compare mode, exporting the
  current comparison as a shareable report. (#960)
- Viewer: support for arbitrary "simple capture" parquets, with
  per-source detection and a Metrics browser for exploring their
  columns. (#989)

### Fixed

- Viewer: live mode now refreshes metadata so the chart query window
  tracks the TSDB as it grows. (#974)
- BPF: samplers now emit a warning when a program fails to attach due to
  a missing kernel symbol, instead of failing silently. (#972)

### Changed

- Every subcommand's `--help` output was reworked to be self-explanatory
  and agent-usable, and the README was reorganized to emphasize use
  cases and the full feature set. (#976, #987, #983)

## [5.15.0] - 2026-06-10

### Added

- Agent: resilient BPF attach and sampler status. The BPF builder now
  attaches each program individually and tolerates per-program failures
  (load/verify failures stay fatal), so a single dead probe no longer
  takes down its sibling programs. A new `GET /samplers` endpoint
  reports each sampler as `active`/`disabled`/`failed` with per-program
  attach detail, and recordings capture this status under
  `per_source_metadata.<source>.sampler_status`. (#954)
- GPU (NVIDIA): `gpu_tensor_utilization` now breaks out per-tensor-pipe
  activity via a `pipe` label — `hmma` (FP16/BF16, and FP32 matmul that
  runs as TF32), `imma` (integer), and `dfma` (FP64) — alongside the
  existing aggregate (`pipe=any`). Collected from NVML GPM, so it
  requires Hopper+ and is reported only where the corresponding pipe is
  supported. (#946)

### Fixed

- BPF samplers that rely on in-kernel BTF (`cpu_usage`, `cpu_migrations`,
  `cpu_perf`, `scheduler_runqueue`, `syscall_counts`) now work on kernels
  built without `/sys/kernel/btf/vmlinux` (e.g. NVIDIA Tegra/L4T). Each
  `tp_btf` hook gains a `raw_tp` twin selected at runtime via
  `kernel_has_btf()`, and `syscall_counts` uses `bpf_get_current_task()`
  instead of `bpf_get_current_task_btf()`. CO-RE still uses the external
  BTF file (`btf_path`). Stock BTF kernels are unaffected. (#948)
- BPF sampler correctness fixes from a full review against
  `docs/principles.md`: histogram bucketing used 32-bit shifts,
  mis-bucketing values ≥ 2³¹ (long-tail latencies ≥ ~2.15 s were
  misreported); blockio latency tracking silently dropped all requests
  on kernels < 5.11 due to a tracepoint argument layout difference;
  scheduler/runqueue could charge runqueue-wait and off-cpu time to the
  wrong cgroup; a full ringbuf no longer permanently suppresses a
  cgroup's name; `tcp_retransmit` now counts segments instead of calls
  (it undercounted with TSO/GSO); plus smaller metadata, histogram, and
  defensive-check fixes. (#956)

## [5.14.0] - 2026-06-03

### Added

- Viewer: Notebook / Selection / Report rework — Markdown notes,
  preamble, and editable titles; report-mode load with column-trim;
  SelectionView mirrors live compare mode. (#908 #909 #919 #931)
- Viewer: event annotations — add from chart tooltips, plus one-off
  event annotations baked into recordings. (#914 #929)
- Viewer: `<rezolus-chart>` embed component for charts with exact
  viewer parity. (#915)
- Dashboard: histogram rate|mean card pair and additional vLLM
  metrics. (#918 #938)
- WASM viewer: Save as Report works offline; restored Load Parquet
  upload affordance. (#923 #924)
- Parquet: combined A/B tarball artifact. (#912)

### Changed

- Adopt `metriken-query` 0.11 streaming `ParquetReader`. (#943)
- Refresh cargo dependencies. (#930)
- Viewer: refactor JS into domain subdirectories, trim redundant
  JS/Rust comments, and a Lit chart spike with CSS tokens /
  `charts.css` extraction. (#913 #932 #933)
- Trim parquet codecs to zstd-only and drop brotli. (#907)
- CI: drop release-profile builds from PR CI; generate A/B smoke
  fixtures on demand instead of checking them in. (#916 #917)

### Fixed

- Viewer: compare-mode bridge KPIs render both arms in section +
  Notebook. (#910)
- Viewer: narrow-screen fixes — topnav cleanup, and chart title-row +
  spectrum controls. (#925 #926)
- Viewer: add missing vllm-prefill / vllm-decode template symlinks.
  (#928)
- CI: locate the viewer tarball regardless of download nesting. (#905)

## [5.13.0] - 2026-05-10

### Added

- BPF: `blockio_requests` sampler now also reports per-op error counts
  (with a coarse class derived from `blk_status_t` — io / timeout /
  nospc / target / protection / unsupported / other) and requeue
  counts, exposed as `blockio_errors` and `blockio_requeues`. The new
  counters are folded into the existing sampler so the
  `block_rq_complete` hot path stays single-attach. (#873)
- Dashboard: new "Exceptions" section consolidating cross-subsystem
  fault signals — network drops & transmit faults, TCP retransmits,
  AWS ENA allowance hits, CFS bandwidth throttling, and the new block
  IO errors / requeues. (#873)
- Viewer (static site): `?capture=alias=URL` now accepts absolute
  http(s) URLs in addition to bundled-demo paths, with comma-separated
  pairs routing straight to A/B compare mode. Landing page split into
  Demos / Explore sections with a "Load from URL" textarea. (#886)
- Viewer (server): opt-in URL-fetch proxy via `--proxy-allow
  <HOST_PATTERN>` (repeatable, shell-style with `*` matching a single
  DNS label) plus a `--proxy-allow-any` escape hatch. Adds
  `POST /api/v1/load_url` so the browser can hand off remote parquet
  captures the server fetches and ingests on its behalf; the landing
  page greys out URL loading when no allowlist is configured and
  prints a warning at startup in any-mode. (#888)
- Viewer: tri-state `urlLoading` (`disabled` / `proxy` / `direct`)
  exposed via `/api/v1/mode` so the static-site and binary frontends
  surface accurate hints about how URL fetches will be performed.
  (#888)
- CI: release builds publish a self-contained
  `rezolus-viewer-vX.Y.Z.tar.gz` bundle (WASM + frontend assets, demo
  parquets excluded, symlinks dereferenced) alongside the deb/rpm
  packages. (#885)
- Tests: end-to-end `tests/viewer_smoke.sh` exercising `rezolus view`
  in upload-only, file, A/B, and proxy modes against checked-in demo
  parquets, plus a companion `viewer-smoke` skill for triage. (#893)
- Docs: `docs/principles.md` capturing the design rules Rezolus
  commits to for BPF samplers (always-on production use, in-kernel
  aggregation read via mmap, H2 histograms, etc.) and the operational
  checklist for reviewing or writing a sampler. (#887)

### Changed

- Viewer: split the 2.6k-line `src/viewer/mod.rs` into focused
  submodules (`state.rs`, `metadata.rs`, `routes.rs`, `actions.rs`).
  No behavior change. (#892)
- Viewer: multi-line chart palette swapped to d3 `schemeCategory10` /
  `schemeCategory20` for stronger discriminability on dark
  backgrounds; muted brown / gray pairs dropped from the cgroup color
  rotation. Per-chart description text now renders as a sibling block
  above the chart card. (#873)
- Bumped direct deps `arrow` / `parquet` 58.1.0 → 58.2.0 and `tokio`
  1.52.1 → 1.52.2; refreshed `Cargo.lock` for in-range bumps across
  `tower-http`, `wasm-bindgen`, `web-sys`, `webpki-root-certs`, the
  `arrow-*` transitive set, and others. Trimmed unused features
  (tokio's `full` → eight specific features, dropped axum's `http2`
  feature and the standalone `h2` direct dep), shrinking the
  transitive crate count. (#891)
- Viewer (server): enable reqwest's `rustls` feature so
  `/api/v1/load_url` can fetch HTTPS URLs. (#888)

### Fixed

- Viewer: cgroup selector columns now truncate long names with
  ellipsis instead of widening past the flex split. (#873)

## [5.12.1] - 2026-05-10

### Fixed

- Viewer: align compare-mode spectrum fetches with a shared step so the
  diff path on quantile heatmaps actually renders, instead of falling
  back to the baseline percentile scatter when the two captures have
  different durations. (#881)
- Viewer: pinned histogram (percentile-scatter) charts in the Selection
  section now wrap their stored query through `buildEffectiveQuery`,
  fixing the empty-card regression where the raw metric name was being
  fetched without the `histogram_quantiles(...)` wrapping. (#882)
- Viewer: persisted selection + notes now actually survive a page
  refresh — `setStorageScope` was wiping the just-bound file-scoped
  localStorage key on every load. (#882)
- Viewer: selection cards now show "Section: Group: Title" so pinned
  charts carry the same context the dashboard headers provide; spectrum
  controls use the narrow layout when rendered inside a selection card
  so they don't crowd the legend. (#882)
- CI: Homebrew formula bump on release no longer fails on permissions.
  (#880)

## [5.12.0] - 2026-05-09

### Added

- Viewer A/B compare mode for two parquet captures, with shared cursor/zoom,
  diff heatmaps, and a compact top-nav compare badge. (#820 #821 #822 #823
  #827 #828 #832)
- Quantile heatmap chart for histogram metrics, with Full/Tail toggles and a
  vertical color legend. Available in compare (A/B) mode with a unified scale
  and diff variant. (#868 #870 #872)
- Service templates for inference-library (vllm + sglang) and
  sglang-router/prefill/decode. Service templates are now embedded in the
  release binary. (#829 #836 #850)
- Per-CPU runqueue wait and context switch metrics in the dashboard. (#875)
- Site landing-page demos: grouped + polished layout and an inference A/B
  example. (#824 #830 #831)
- Viewer URL params honor `capture=alias=path`, scaling to N captures, with a
  matching positional CLI. (#827 #832)
- Recorder auto-labels `source=rezolus` when probing a Msgpack endpoint, so
  `--metadata source=rezolus` is no longer required for local agents. (#878)
- Parquet `annotate` gains `--node`, `--source`, and systeminfo JSON
  annotation; `--source` rename now updates matching `per_source_metadata`
  keys. (#840 #841 #853)
- Viewer: chart controls moved to top-right with icon-only buttons, and
  unavailable charts listed at the bottom of regular sections. (#859 #869)
- Viewer WASM is built during the GitHub Pages deploy. (#863)

### Changed

- Viewer: lazy section generation in two phases — split sections metadata
  from payloads and defer payload construction until a section is rendered.
  Reduces initial chart load memory and time. (#835 #848 #851)
- Viewer: observable `ChartsState` with zoom subscriptions; percentile pins
  stay local per chart. (#825 #828)
- Viewer: rename `histogram_percentiles` → `histogram_quantiles` and drop the
  `percentile` label. (#856)
- Dashboard: suppress per-device charts on single-GPU and single-CPU
  recordings. (#864)
- Bumped `metriken-query` 0.9.5 → 0.10.2 — 0.9.6 tightens load-path heap
  allocations (60–90% reduction on demo parquets, with a corresponding drop
  in the WASM viewer's memory footprint), and 0.10.x ships the streaming
  PromQL engine. Also bumped `histogram` and `wasm-bindgen`, and refreshed
  the rest of the cargo dependencies. (#839 #858)
- Marked the `rezolus` crate as unpublishable; prepared the `systeminfo`
  crate for publishing. (#771 #871)

### Fixed

- Viewer: preserve deferred per-cgroup charts when section payload filtering
  runs before the cgroup selector populates them. (#874)
- Viewer: missing histograms for service metrics. (#862)
- Viewer: compare-mode bug bash — legends, zoom, layout, default route — and
  follow-ups including diff-heatmap polish, narrow-screen time range bar, and
  pinning the node filter to both captures. (#821 #822 #823 #827 #852 #854)
- Viewer: redirect stale `#/service/...` hashes to the default route, and
  break the redirect loop when the default route itself is missing.
  (#842 #843)
- Viewer: drive template + category membership from the parquet source,
  refresh experiment charts when granularity changes, and fix WASM template
  parsing for inference-library. (#833 #838 #846)
- Viewer: multi-node selector tappable on mobile. (#865)
- WASM: rebuild plus a CI gate against bad externref-table layout. (#847)
- Parquet: flatten existing source arrays when combining, and expand array
  sources into `per_source_metadata`. (#845 #849)
- Installer: run as root and create the `rezolus` user on RPM systems. (#844)

## [5.11.0] - 2026-04-21

### Added

- Dashboard: subgroups and full-width plots. (#815)
- Viewer: preserve step override across save/load of selection. (#817)

### Fixed

- Agent: emit histograms in snapshots after metriken 0.2 core bump.
  Recordings made since 5.10.0 were missing all histogram columns. (#818)
- Viewer: hide subgroup header when all charts are empty. (#816)

## [5.10.0] - 2026-04-19

### Added

- Parquet subcommands: `metadata` to inspect files (#776), `annotate` to embed
  service extension KPIs (#767), `combine` to merge multi-source recordings
  (#777), and `filter` to reduce files to a needed subset of columns
  (#779 #781).
- Recorder support for multiple endpoints in a single recording. (#793)
- Multi-node viewer with combine/process monitoring modes and UX improvements.
  (#785 #795)
- WASM viewer crate imported from `rezolus-webview` for the browser-only
  static site viewer. (#800)
- Docker support with agent container and standalone `rezolus-capture` script.
  (#782 #783 #797)
- Service extension framework with runtime-loaded templates for SGLang,
  valkey, vllm, and cachecannon. Multiple service dashboard sections can be
  rendered per recording. (#774 #786 #788 #789 #794 #799)
- GPU dashboard improvements including PCIe bandwidth utilization. (#812)
- Viewer: granularity (step) selector to override query step (#773), splash
  screen and dashboard labels (#809), styled endpoints and script
  consolidation (#808), unit selector (#802), stable cgroup colors with
  selector swatches (#798).
- Support for multiple service endpoints and process monitoring modes. (#785)

### Changed

- Refactored agent to use metriken group types instead of local
  implementations. (#796)
- Unified dashboard JSON via a shared `dashboard` crate. (#806)
- Extracted shared Group component and helpers into `viewer_core.js`. (#804)
- Bumped metriken to 0.9.2 for GaugeGroup sentinel fix. (#811)
- Bumped metriken to crates.io and arrow/parquet to 58. (#803)

### Fixed

- Viewer: topology and sysinfo layout on narrow screens. (#810)
- Viewer: cgroup charts survive granularity change. (#801)
- Viewer: expand button URLs for site hash-based routing. (#791)
- Viewer: stable cgroup colors, swatches in selector, clear ghost legends.
  (#798)
- Parquet `filter` retains histogram columns. (#781)
- Selection/report localStorage scoped by file content fingerprint. (#790)
- Symlinked templates, WASM rebuild, and pre-commit hook for viewer assets.
  (#805)
- Inside `dataZoom` removed so charts no longer capture scroll events. (#792)

## [5.9.1] - 2026-04-12

### Fixed

- CI: install gnupg2 and pinentry for RPM signing in containers.

## [5.9.0] - 2026-04-10

### Added

- Light theme with automatic dark/light toggle. (#762)
- GPU Activity section showing GPM metrics. (#760)
- Viewer landing page with drag-and-drop file upload and live agent connect. (#753)
- Auto-generate dashboard JSON from Rust definitions. (#751)
- Interactive WASM viewer with demo on project site. (#739 #740)
- Save-as modal, toast notifications, and report metadata in viewer. (#738)
- Recorder `--metadata key=value` option for parquet metadata. (#737)
- Recorder support for recording from Prometheus endpoints. (#736)
- Selection section for curated chart collections in viewer. (#735)
- Interactive CPU topology diagram in viewer System Info. (#731)
- Serve and fetch systeminfo from agent. (#730)
- CPU topology, NIC summary, and NUMA locality in systeminfo. (#726)
- Embed system hardware summary in parquet metadata. (#724)
- GPG signing for RPM packages and release checksums. (#721)
- Integration tests for BPF samplers. (#720)
- Unit tests for metric group types. (#719)
- Project site. (#712)

### Changed

- Refactor metric types from style-based to semantic type system. (#766)
- Gate dump-dashboards behind xtask-commands feature flag. (#764)
- Replace ringlog with tracing ecosystem. (#715)
- Replace duplicated viewer lib files with symlinks to src/viewer/assets/lib. (#746)
- Viewer improvements: per-chart range clamping with OOB band (#741), time range
  bar UX (#733), Y-axis rescaling, no-data collapse, metadata, cgroup selector
  fix (#728), two-column layout for cgroups (#725), chart navigation and layout
  (#729), mobile-responsive layout and cgroup paired charts, API responses and
  topology rendering (#732), UI layout and descriptions (#713).

### Fixed

- Viewer: cgroup fixes, legend layout overhaul, and chart cleanup. (#765)
- Viewer: theme toggle placement and icon polish. (#763)
- Viewer: narrow-screen legend for heatmap charts and colon title. (#759)
- Viewer: hide chart buttons and add scroll gutter on touch devices. (#756)
- Viewer: responsive layout for narrow viewports. (#755)
- Replace out-of-tree import with local symlink in site. (#750)
- Pair cgroup charts by title instead of array index in viewer.
- Reduce memory footprint for per-task CPU usage metrics. (#718)
- Serve packed counter values directly from BPF mmap. (#722)
- CI: skip release/pages/tag workflows on forks (#744), dereference symlinks
  for GitHub Pages (#748), move GPG key check to shell guard (#749).
- Use tarball URL for Homebrew formula updates. (#723)

## [5.8.2] - 2026-03-19

### Fixed

- Use `bpf_ringbuf_reserve`/`submit` instead of stack allocation with
  `bpf_ringbuf_output` across all BPF programs. Fixes BPF stack overflow
  on newer kernels where combined stack size exceeded the 512-byte limit. (#710)

## [5.8.1] - 2026-03-16

### Fixed

- Remove unused BPF function requiring clang 15+. (#708)

## [5.8.0] - 2026-03-16

### Added

- Live mode for the viewer. (#704)

### Changed

- Use the __parent field to travel the cgroup tree. (#705)

## [5.7.0] - 2026-03-10

### Added

- Ensure tooltip background is present so that its text remains legible. (#697)

### Fixed

- Bump metriken-query to 0.2.0 which fixes histogram_quantile() semantics. (#699)
- Disable "Reset Zoom" button when already zoomed out. (#696)
- Fix charts shifting on hover.

## [5.6.0] - 2026-02-26

### Added

- GPU GPM metrics for Hopper+ GPUs. (#689)
- Socket group and socket mode configuration for external metrics socket. (#687)
- Network ethtool sampler for ENA allowance counters. (#683)
- Unix domain socket external metrics ingestion. (#677)
- SparseCounterGroup for high-cardinality metrics. (#676)
- Per-task CPU usage metrics. (#675)
- GPU metrics dashboard section in viewer. (#673)
- CUPTI PM sampling for GPU metrics. (#671)

### Changed

- Viewer uses metriken-query crate for TSDB and PromQL. (#678)
- Viewer style improvements. (#668 #674)

## [5.5.0] - 2026-01-23

### Added

- Memory section and network errors added to the viewer. (#637)
- Use stability analysis to improve anomaly detection in mcp server. (#639)
- Macos GPU sampler. (#642)
- Hindsight HTTP endpoint for snapshotting. (#652)
- Perf counters for branch prediction and DTLB. (#655)
- Improved support for CPU L3 sampler. (#664)


## [5.4.0] - 2025-09-30

### Fixed

- Fixes CPU IPNS, Frequency, and complex user queries in the viewer. (#629)

### Added

- Homebrew and Debian Trixie packaging jobs (#595 #612)
- Installation script and documentation (#613)
- MCP server for LLM guided analysis of recordings. (#614 #617 #618 #622)
- Support for external BTF. (#620)

## [5.3.1] - 2025-08-21

### Fixed

- Fixed RPM packaging to enable publishing to GCP Artifact Registry. (#609)

## [5.3.0] - 2025-08-20

### Added

- Improved cgroup exploration and a query explorer for Rezolus Viewer. (#604)

## [5.2.3] - 2025-08-04

### Fixes

- Fixes logging from libbpf so it is correctly directed to the debug log. (#597)

## [5.2.2] - 2025-07-28

### Fixes

- Fixes cpu usage sampler reporting incorrect values on Ubuntu 24 (noble)
  when installed from our deb packages, or when built on systems with clang-18.
  (#585)

## [5.2.1] - 2025-07-02

### Fixes

- Fixes scheduler runqueue sampler failing the BPF verifier on some systems
  since 5.2.0 (#576)

## [5.2.0] - 2025-07-01

### Added

- Adds per-cgroup context switch, runqueue wait, and off-cpu time. Adds runqueue
  waiting time on a per-CPU basis. (#568)

### Fixes

- Fixes CPU usage accounting bug when PIDs are re-used. (#571)

## [5.1.0] - 2025-06-24

### Changed

- Replaced `network_interfaces` sampler with a BPF based sampler. Results in
  changes to some network metrics. Improves sampling latency. (#565)

### Added

- Snapshot caching with TTL added to the agent. (#553)
- Rezolus viewer has more charts. (#554 #561)
- Realtime scheduling for the agent to improve sampling latency. (#559)
- Virtualization aware optimization for perf counter collection. (#548)
- Additional cgroup charts in Rezolus viewer. (#564)

### Fixed

- Softirq time added to system time when running in idle task context. (#556)

## [5.0.1] - 2025-06-13

### Fixed

- Rezolus Viewer served a 404 instead of using embedded index html. (#550)

## [5.0.0] - 2025-06-11

### Changed

- Many metrics have been renamed to use labels to track different categories
  instead of encoding that in the metric name.
- Removed any system-wide total metrics when summing across some metric would
  give the same result.
- BlockIO latency and size distributions split by operation. (#380 #381)
- CPU performance (instructions, cycles) tracking changed to allow using the
  same perf counters for tracking these per-cgroup. May result in slight skew
  since these are no-longer a grouped read. (#383)
- Removed TCP connection state sampler due to high CPU usage. (#456)
- Prometheus exposition moved out into a separate service. (#470)

### Added

- Recorder extended with additional options. (#387 #389)
- Hindsight recorder for always-on ringbuffer recording. (#409 #436 )
- Per-cgroup syscalls. (#419)
- Per-cgroup cpu usage. (#420 #425)
- SoftIRQ time tracked by IRQ kind. (#462)
- Sampler for TLB flush events. (#467)
- Softnet sampler for understanding kernel packet processing. (#480)
- Additional syscall groups. (#482 #503)
- CPU CFS throttling metrics for cgroups. (#488)
- CPU L3 Cache Hit/Access (AMD Zen only). (#487)
- CPU Migrations. (#491)
- Rezolus Viewer for opening parquet files. (#530)
- JSON endpoint for Rezolus Agent. (#532)
- BPF program stats. (#533)
- Per-sampler latency logging. (#535)

### Fixed

- Set open file limit for agent sufficiently high for large systems. (#511)
- Changed recorder missed tick behavior from burst to skip. (#513)

## [4.1.2] - 2024-11-25

### Fixed

- Fixed VM size for RPM packaging. (#376)
- Various dependencies updated.

## [4.1.1] - 2024-10-24

### Fixed

- Fixed packaging workflow so DEB and RPM artifacts are published. (#365)

## [4.1.0] - 2024-10-23

### Added

- `rezolus-recorder` which can connect to a running Rezolus instance and record
  metrics into a parquet file. (#356)

### Fixed

- Configured listen address was ignored and a default was used. (#362)

## [4.0.0] - 2024-09-27

### Changed

- All samplers are now async and run on-demand when a metrics endpoint is hit.
  Percentiles must now be calculated from the free-running histograms. (#334)
- BPF is now non-optional on Linux. (#336)
- Makes HTTP compression dependent on client accept headers, removes the need
  for separate configuration of compression. (#337)
- Removes histograms for counter and gauge metrics. (#338)
- Histogram grouping power reduced from 7 (0.781% error) to 3 (12.5% error).
  This reduces the memory footprint for each histogram. (#339)

### Added

- Syscall samplers now track yield syscall. (#310)
- TCP active open (connect) latency now tracked. (#342)

## [3.18.1] - 2024-08-09

### Changed

- Ubuntu Bionic dropped from packaging job. (#306)

## [3.18.0] - 2024-08-09

### Fixed

- CPU per core cycle and instruction counters were incorrect. The system totals
  reflected the appropriate value. (#304)

### Added

- Allow setting the metric snapshot interval to better address cases where
  metrics are not scraped on a per-second basis. (#286)
- Network carrier change metric. (#290)

### Changed

- Explicitly use relaxed atomics in BPF samplers. (#276)
- Split syscall sampler into syscall_latency and syscall_counts to allow more
  fine-grained configuration and allow for reduced overhead. (#298)
- Split block_io latency sampler into block_io_latency and block_io_requests to
  allow more fine-grained configuration and allow for reduced overhead. (#299)

## [3.17.0] - 2024-06-27

### Fixed

- Fixes sampler configuration inheritance for partially defined sampler config
  sections (#273)
- Linux BPF CPU usage sampler had idle time metric counting backwards (#281)

### Added

- Fallback for linux network traffic sampler to get metrics from sysfs when BPF
  is not enabled or fails to initialize (#274)
- JSON stats endpoints (#277)

## [3.16.0] - 2024-05-14

### Fixed

- Listen address could not be changed (#264)

### Added

- CPU busy time metric for non-Prometheus endpoints (#256)

## [3.15.0] - 2024-05-09

### Fixed

- CPU usage sampler could not be disabled (#259)
- Example config out-of-date (#260)

### Added

- Example Grafana/Prometheus dashboard (#239)
- Off-CPU time added to the scheduler runqueue sampler (#240)
- Filesystem sampler added and tracks the number of open file descriptors (#242)
- Log level can now be set in the config (#246)
- Network interface statistics for tx/rx errors (drop, crc, ...) (#247)
- BPF can be enabled/disabled per-sampler or globally. (#258)

## [3.14.2] - 2024-04-18

### Fixed

- CPU usage for soft and hard irq was incorrectly reported. (#236)

## [3.14.1] - 2024-04-16

### Fixed

- CPU usage reporting via BPF would report CPU as always idle on some systems.
  (#233)

## [3.14.0] - 2024-04-03

### Changed

- metriken crates updated which changes the msgpack output. (#224)

## Fixed

- Dependency updates to address RUSTSEC-2024-0332.

## [3.13.0] - 2024-04-01

### Changed

- Memory sampler was reporting memory usage stats in KiB, but with bytes for the
  unit metadata. This change corrects the sampler to report memory usage in
  bytes. This fix is disruptive as it will cause the memory stats to change.
  (#222)

## [3.12.0] - 2024-03-28

### Added

- MacOS cpu usage sampling. (#203)
- Metric unit annotations are added and exposed as metadata.
- Logs version number on startup. (#213)

### Fixed

- Incorrect summary stats (percentiles) were reported in version 3.10.2, 3.10.3,
  and 3.11.0. (#216)

## [3.11.0] - 2024-03-25

### Changed

- Refactored the scheduler and syscall BPF samplers to reduce overheads. (#193
  #195)

### Added

- BlockIO thoughput and operation metrics using BPF. (#198)
- Network throughput and packet metrics using BPF. (#200)

### Fixed

- Online CPU detection for CPU usage sampler needed a trimmed string. (#194)

## [3.10.3] - 2024-03-20

### Fixed

- Fixes an incorrect calculation of the number of online CPUs in the BPF-based
  CPU usage sampler.

## [3.10.2] - 2024-03-20

### Fixed

- Fixes a panic in the CPU perf event sampler due to a divide-by-zero. This
  occurs when there are no active perf event groups. (#185)

## [3.10.1] - 2024-03-20

### Fixed

- Fixes per-CPU idle time accounting in the BPF-based sampler. Starting in
  release 3.9.0 these metrics incorrectly report no idle time. (#181)

## [3.10.0] - 2024-03-19

### Added

- Additional system information fields including kernel version, CPU frequency
  details, network queues, and IRQ affinity. (#100)

### Fixed

- Fixes a panic on some systems when perf counter initialization has failed.
  This bug was introduced in 3.9.0. (#175)
- Fixes CPU idle time accounting in the BPF-based sampler. In 3.9.0 the sampler
  incorrectly reports no idle time. (#176)

## [3.9.0] - 2024-03-15

### Added

- CPU usage metrics are now collected via BPF when available. (#165)
- Perf event sampler can now initialize when only some counters are available.
  (#168)

## [3.8.0] - 2024-03-04

### Added

- Allows Rezolus to run on MacOS though sampler support is limited.
- Provides msgpack exposition format as a more efficient exposition format.

### Fixed

- Updates of various direct dependencies.

## [3.7.0] - 2023-12-21

### Added

- Optional compression for HTTP exposition. (#128)
- Additional GPU metrics for utilization and energy consumption. (#138)

### Fixed

- Duplicate metric name in Rezolus sampler. (#134)

## [3.6.1] - 2023-11-30

### Fixed

- Fixed incorrect type annotation for CPU metrics (frequency, ipkc, ipus). (#98)
- Fixed under-reported TCP retransmits. (#121)
- Fixed TCP segment metrics. (#123)

## [3.6.0] - 2023-10-26

### Added

- Allow configuration of individual samplers in the config file. This allows
  each sampler to be individually enabled/disabled and have its collection
  intervals adjusted.
- TCP connection state sampler which tracks the number of tcp connections in
  each state.
- Rezolus sampler which monitors resource utilization of Rezolus itself.
- Optional exposition of histogram buckets on the Prometheus/OpenTelemetry
  endpoint.
- Track latencies for each group of syscalls to help understand the breakdown of
  total syscall latency.

### Fixed

- Corrected a length check of the mmap'd histogram regions. This fix enables the
  fast path for reading histogram data into userspace.

## [3.5.0] - 2023-10-16

### Changed

- Updated `metriken` and replaced heatmaps with histograms. This reduces runtime
  resource utilization.

## [3.4.0] - 2023-10-10

### Changed

- Moved to fetching multiple percentiles at once to reduce overhead.
- Refactor of the hardware info sampler into a separate crate to allow reuse and
  make improvements to that sampler.

### Fixed

- Update `warp` to address RUSTSEC-2023-0065.

## [3.3.3] - 2023-08-08

### Added

- Packaging support for `aarch64`

### Fixed

- Updated dependencies to pull-in fixes and improvements.

## [3.3.2] - 2023-08-08

### Fixed

- Fixed hardware info and cpu samplers on platforms which do not expose either
  die or node information in the topology, which may happen on ARM.
- Fixed BPF program generation to restore compatibility with clang 11.

## [3.3.1] - 2023-08-07

### Fixed

- Fixed path inconsistency in Debian packaging.

## [3.3.0] - 2023-08-02

### Added

- Added BTF type definitions for aarch64 target architecture.

### Fixed

- Update dependencies to reduce overhead and pull-in bugfixes.
- Documentation improvements.

## [3.2.0] - 2023-07-26

### Added

- Added a TCP packet latency sampler to measure the latency from packet being
  received to being processed by the userspace application.
- Added per-device metrics for GPU sampler.

## [3.1.0] - 2023-07-26

### Added

- Added per-CPU metrics for usage, frequency, and perf counters.
- Added BPF to the set of default features.

## [3.0.0] - 2023-07-25

### Changed

- Rewritten implementation of Rezolus using libbpf-rs and perf-event2 to provide
  a more modern approach to BPF and Perf Event instrumentation. 

[unreleased]: https://github.com/iopsystems/rezolus/compare/v5.21.0...HEAD
[5.21.0]: https://github.com/iopsystems/rezolus/compare/v5.20.0...v5.21.0
[5.20.0]: https://github.com/iopsystems/rezolus/compare/v5.19.1...v5.20.0
[5.19.1]: https://github.com/iopsystems/rezolus/compare/v5.19.0...v5.19.1
[5.19.0]: https://github.com/iopsystems/rezolus/compare/v5.18.0...v5.19.0
[5.18.0]: https://github.com/iopsystems/rezolus/compare/v5.17.0...v5.18.0
[5.17.0]: https://github.com/iopsystems/rezolus/compare/v5.16.2...v5.17.0
[5.16.2]: https://github.com/iopsystems/rezolus/compare/v5.16.1...v5.16.2
[5.16.1]: https://github.com/iopsystems/rezolus/compare/v5.16.0...v5.16.1
[5.16.0]: https://github.com/iopsystems/rezolus/compare/v5.15.0...v5.16.0
[5.15.0]: https://github.com/iopsystems/rezolus/compare/v5.14.0...v5.15.0
[5.14.0]: https://github.com/iopsystems/rezolus/compare/v5.13.0...v5.14.0
[5.13.0]: https://github.com/iopsystems/rezolus/compare/v5.12.1...v5.13.0
[5.12.1]: https://github.com/iopsystems/rezolus/compare/v5.12.0...v5.12.1
[5.12.0]: https://github.com/iopsystems/rezolus/compare/v5.11.0...v5.12.0
[5.11.0]: https://github.com/iopsystems/rezolus/compare/v5.10.0...v5.11.0
[5.10.0]: https://github.com/iopsystems/rezolus/compare/v5.9.1...v5.10.0
[5.9.1]: https://github.com/iopsystems/rezolus/compare/v5.9.0...v5.9.1
[5.9.0]: https://github.com/iopsystems/rezolus/compare/v5.8.2...v5.9.0
[5.8.2]: https://github.com/iopsystems/rezolus/compare/v5.8.1...v5.8.2
[5.8.1]: https://github.com/iopsystems/rezolus/compare/v5.8.0...v5.8.1
[5.8.0]: https://github.com/iopsystems/rezolus/compare/v5.7.0...v5.8.0
[5.7.0]: https://github.com/iopsystems/rezolus/compare/v5.6.0...v5.7.0
[5.6.0]: https://github.com/iopsystems/rezolus/compare/v5.5.0...v5.6.0
[5.5.0]: https://github.com/iopsystems/rezolus/compare/v5.4.0...v5.5.0
[5.4.0]: https://github.com/iopsystems/rezolus/compare/v5.3.1...v5.4.0
[5.3.1]: https://github.com/iopsystems/rezolus/compare/v5.3.0...v5.3.1
[5.3.0]: https://github.com/iopsystems/rezolus/compare/v5.2.3...v5.3.0
[5.2.3]: https://github.com/iopsystems/rezolus/compare/v5.2.2...v5.2.3
[5.2.2]: https://github.com/iopsystems/rezolus/compare/v5.2.1...v5.2.2
[5.2.1]: https://github.com/iopsystems/rezolus/compare/v5.2.0...v5.2.1
[5.2.0]: https://github.com/iopsystems/rezolus/compare/v5.1.0...v5.2.0
[5.1.0]: https://github.com/iopsystems/rezolus/compare/v5.0.1...v5.1.0
[5.0.1]: https://github.com/iopsystems/rezolus/compare/v5.0.0...v5.0.1
[5.0.0]: https://github.com/iopsystems/rezolus/compare/v4.1.2...v5.0.0
[4.1.2]: https://github.com/iopsystems/rezolus/compare/v4.1.1...v4.1.2
[4.1.1]: https://github.com/iopsystems/rezolus/compare/v4.1.0...v4.1.1
[4.1.0]: https://github.com/iopsystems/rezolus/compare/v4.0.0...v4.1.0
[4.0.0]: https://github.com/iopsystems/rezolus/compare/v3.18.1...v4.0.0
[3.18.1]: https://github.com/iopsystems/rezolus/compare/v3.18.0...v3.18.1
[3.18.0]: https://github.com/iopsystems/rezolus/compare/v3.17.0...v3.18.0
[3.17.0]: https://github.com/iopsystems/rezolus/compare/v3.16.0...v3.17.0
[3.16.0]: https://github.com/iopsystems/rezolus/compare/v3.15.0...v3.16.0
[3.15.0]: https://github.com/iopsystems/rezolus/compare/v3.14.2...v3.15.0
[3.14.2]: https://github.com/iopsystems/rezolus/compare/v3.14.1...v3.14.2
[3.14.1]: https://github.com/iopsystems/rezolus/compare/v3.14.0...v3.14.1
[3.14.0]: https://github.com/iopsystems/rezolus/compare/v3.13.0...v3.14.0
[3.13.0]: https://github.com/iopsystems/rezolus/compare/v3.12.0...v3.13.0
[3.12.0]: https://github.com/iopsystems/rezolus/compare/v3.11.0...v3.12.0
[3.11.0]: https://github.com/iopsystems/rezolus/compare/v3.10.3...v3.11.0
[3.10.3]: https://github.com/iopsystems/rezolus/compare/v3.10.2...v3.10.3
[3.10.2]: https://github.com/iopsystems/rezolus/compare/v3.10.1...v3.10.2
[3.10.1]: https://github.com/iopsystems/rezolus/compare/v3.10.0...v3.10.1
[3.10.0]: https://github.com/iopsystems/rezolus/compare/v3.9.0...v3.10.0
[3.9.0]: https://github.com/iopsystems/rezolus/compare/v3.8.0...v3.9.0
[3.8.0]: https://github.com/iopsystems/rezolus/compare/v3.7.0...v3.8.0
[3.7.0]: https://github.com/iopsystems/rezolus/compare/v3.6.1...v3.7.0
[3.6.1]: https://github.com/iopsystems/rezolus/compare/v3.6.0...v3.6.1
[3.6.0]: https://github.com/iopsystems/rezolus/compare/v3.5.0...v3.6.0
[3.5.0]: https://github.com/iopsystems/rezolus/compare/v3.4.0...v3.5.0
[3.4.0]: https://github.com/iopsystems/rezolus/compare/v3.3.3...v3.4.0
[3.3.3]: https://github.com/iopsystems/rezolus/compare/v3.3.2...v3.3.3
[3.3.2]: https://github.com/iopsystems/rezolus/compare/v3.3.1...v3.3.2
[3.3.1]: https://github.com/iopsystems/rezolus/compare/v3.3.0...v3.3.1
[3.3.0]: https://github.com/iopsystems/rezolus/compare/v3.2.0...v3.3.0
[3.2.0]: https://github.com/iopsystems/rezolus/compare/v3.1.0...v3.2.0
[3.1.0]: https://github.com/iopsystems/rezolus/compare/v3.0.0...v3.1.0
[3.0.0]: https://github.com/iopsystems/rezolus/releases/tag/v3.0.0
