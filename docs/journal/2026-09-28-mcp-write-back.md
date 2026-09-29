# MCP write-back and tool tiers

- **Opened:** 2026-09-28
- **Status:** IN PROGRESS. Tiers, `add_event`, `run_checks` and
  `remove_events` built (#1358); `export_query` and `viewer_link` built
  (second PR); `rezolus mcp install` and the skill follow.

## Problem

The MCP server exposes six tools (`src/mcp/server.rs`): `describe_recording`,
`describe_metrics`, `query`, `detect_anomalies`, `analyze_correlation`,
`extract_features`. All read. An agent that finds a latency spike at
14:03:11 can describe it in its reply and nothing else: it cannot mark the
recording, it cannot hand the person a link to the chart, and the next agent
to open the recording starts from zero.

Two smaller gaps sit beside that one:

- **Setup is manual.** The stdio server exists; registering it with a client
  and telling the client how to use it is left to the user. There is no
  install step and no skill shipped alongside the server, so every client
  gets the six tool descriptions and nothing about the workflow (call
  `describe_metrics` before `query`, use `sum()` for `detect_anomalies`). The
  descriptions carry some of that today as inline prose, which is the wrong
  place for a workflow.
- **No way to hand data to a local tool.** Every analysis goes through a
  PromQL round trip. An agent that wants a histogram over a raw column, or a
  join the query engine does not express, has no path to the rows.

## Goal

- Additive write tools, on by default.
- Mutating tools, off by default, enabled by a flag.
- A link tool, once links carry state.
- An install command that registers the server and drops a skill.
- A tool that materializes a query result as a file the agent can open.

## Design

**Three tiers, decided by what a tool can destroy.**

| tier | default | tools |
|------|---------|-------|
| read | on | the six existing tools |
| additive | on | `add_event`, `export_query`, `viewer_link` |
| mutating | off, `rezolus mcp --allow-mutating` | `remove_events`, `set_kpis` |

Additive tools can only add to a recording, through the same manifest `UPDATE`
shape `annotate --add-events` uses, so a wrong call is an extra event and not
a lost one. Mutating tools can remove or replace, and stay behind the flag
because an agent acting on a shared recording should not be able to clear
another person's events by default.

**`add_event`** takes the `Event` fields (`crates/dashboard/src/events.rs`),
writes through the manifest path, and returns the event id. `source` is set
to `mcp` unless given, so a person can later filter agent-authored events.
The recording selector (`recording: {source: "redis"}`) applies as on every
other tool.

**`viewer_link`** takes a section or chart id and a time range and returns a
URL for a running viewer, following the parameter set in
[viewer links that carry the whole view](2026-09-28-viewer-link-state.md).
It needs the viewer's address, which the server does not know: a
`--viewer-url` flag on `rezolus mcp`, or the tool returns the hash fragment
alone and the client prepends the host. Decide in the PR; the fragment-only
form works without configuration and is the likely first cut.

**`export_query`** runs a PromQL range query and writes the result as a
parquet or CSV file under a directory the server was started with
(`--export-dir`, refused if absent). Returns the path. This is the escape
from PromQL: the agent gets rows it can load into whatever it has. Bounded by
the same reader-memory path as the viewer.

**`run_checks`** joins the additive tier when
[checks with verdicts](2026-09-28-checks-with-verdicts.md) lands: it runs the
recording's checks and, with `annotate: true`, writes the verdict events.

**`rezolus mcp install`** detects the clients it knows how to register with
(Claude Code and any other whose config format is documented), writes the
server entry, and installs a skill describing the workflow: discovery before
query, aggregation rules for `detect_anomalies`, when to use
`extract_features`, and the recording selector. The inline workflow prose in
the tool descriptions moves into the skill and the descriptions shrink to
what each tool does. `document-feature` applies: the skill is an interface
under test.

## Built: tiers, `add_event`, `run_checks`, `remove_events`

The first PR lands the tier plumbing and the three tools whose write path
already existed.

**Tiers.** `rezolus mcp --allow-mutating` sets `ServerOptions` on the
server (`src/mcp/server.rs`). `tools/list` is built in tiers: the six read
tools, the additive ones, and the mutating ones only when the flag is on. A
mutating call on a flag-off server is refused with a message naming the
flag, tested by
`a_mutating_call_without_the_flag_is_refused_naming_the_flag`. The flag is
refused on the one-shot subcommands, which read.

**`add_event`.** Builds one `Event` from the arguments and appends it
through `parquet_tools::events::add_events_selected`, which resolves the
selector to one recording with the check runner's open (`check::open_targets`)
and writes with the annotation writer's per-recording path (a parquet file
takes the footer path). A multi-recording archive with no selector is
refused with the listing the read tools give: `annotate --event` writes the
same event into every recording, and an agent marking what it saw in one
arm must not stamp the other arms. `source` defaults to `mcp`; the id is
minted as `mcp:<uuid>` (`agent::epoch::mint`, the producer-epoch minter)
unless given, and a repeated id is a no-op through `append_events`' id rule
(a stored `kind=check` event is replaced by one with the same id).
`timestamp` takes RFC 3339 or Unix seconds as a number; `duration` takes
humantime or seconds. A digit-only string is refused for both: under
`annotate --event`'s convention it would read as nanoseconds, a billion
away from the same digits sent as a number, with no error in between
(review found `"1776804000"` landing 1.8 s after the epoch). Numbers past
1e11 s or 1e9 s are refused for the same reason.
The reply carries the id, the ns instant, the outcome, the recording's
event count, and the annotation writer's report line, which is where a
v1/v2 tar archive says it was upgraded to v3 on the way (review found the
upgrade happening silently). Every write evicts the server's cached readers
for that path, keyed by canonical path: `ParquetSource` keeps its footer
offsets from open time and the footer path rewrites the file in place, so a
reader cached under another spelling of the path would decode the new bytes
at the old offsets.

**`run_checks`.** The check runner was split out of its clap wrapper into
`check::run_checks` (evaluation, no I/O beyond the open) and
`check::annotate_events` (the write, returning its report line), and the
CLI and the tool both call them. The tool returns each `CheckResult` as the
CLI's `--json` does, plus a summary, the exit code the CLI would give, and
the annotation report when `annotate` was set. `queries` is a
ServiceExtension object inline, since the agent has JSON in hand and no file
to point at.

**`remove_events`.** A read, a filter (`ids` any-of, `kind`, `source`, all
given fields must match) and a replace. The replace is a new
`RezAnnotation::per_recording_replace` (a whole payload per recording, `None`
to leave one alone), symmetric with `per_recording_events`; the parquet path
rewrites the footer. An empty filter is refused: "remove everything" is
`annotate --clear-events`, typed by a person. The annotation writer's report
line is now a `ReportSink` (stdout, or captured and accumulated: a tar
upgrade reports twice) rather than a stdout/stderr flag, so the tools
return the lines instead of printing them.

`run_checks` with no selector over a multi-recording archive evaluates every
recording and gives each its own verdicts, as `recording check` does; the
two event tools refuse the same call. The difference is deliberate: a
verdict is computed per arm from that arm's data, an event is one mark the
agent saw somewhere.

**Measured.** Through stdio against a two-recording `.rez` combined from
the A/B parquet fixtures, with a `recording` selector on each write:
`initialize`, `tools/list` (eight tools without the flag), `add_event`
(landed in the selected recording, id `mcp:<uuid>` returned),
`remove_events` (refused, names the flag), `run_checks` (no checks to
run). `rezolus view` on the archive afterwards serves the event in
`/api/v1/file_metadata`, which is what the timeline draws from. That is the
entry's first GO condition.

## Built: `export_query` and `viewer_link`

The second PR adds the two tools that do not touch a recording's
metadata, and the two server flags they need.

**`export_query`** (`src/mcp/export.rs`). Runs the query the way the
`query` tool does (whole recording, step 1 s unless `step` is given) and
flattens the result to long form, one row per series and timestamp:
`series` (the label set as the CLI prints it), `timestamp` (Unix
seconds), `value`, and `lo`/`hi` from the acquisition-window band when the
query carried one. CSV and parquet carry the same five columns. A heatmap
result is refused with a pointer at `histogram_quantile`. Files land only
under `rezolus mcp --export-dir DIR`, which must exist at startup; without
the flag the tool is listed and refuses every call naming it. The
filename must be one path component (no separators, no `..`, no leading
dot) or is derived from a hash of the query and the time; an existing file
is never overwritten. The reply carries the path, the counts, the columns
and the evaluated range.

**`viewer_link`** (`src/mcp/link.rs`). Pure formatting of the
[viewer link](2026-09-28-viewer-link-state.md) wire form: the section and
chart in the hash, everything else in the query string, values
percent-encoded as `URLSearchParams` reads them back. `from`/`to` go
together (RFC 3339 or seconds as a number, digit-only strings refused as in
`add_event`), `time=raw` only, a zero anchor is not written. The decision
the entry left open is made both ways: the reply always carries the
fragment and query on their own, and a full URL when the call passes
`viewer_url` or the server was started with `--viewer-url`. The Rust tests
pin one full fixture string, and `tests/viewer_link_parity.test.mjs` parses
that same string with the viewer's `parseViewState` and checks the Rust
source still declares it, so the two sides cannot drift apart without a
failing test.

Both flags, like `--allow-mutating`, are refused on the one-shot
subcommands.

## Not in scope

- A hosted or remote MCP transport. Stdio only.
- Tools that modify the agent's configuration. Different binary mode,
  different trust boundary.

## GO / NO-GO

GO when `add_event` from a stdio client lands an event that the viewer draws
on the next open, and `rezolus mcp install` on a clean machine leaves Claude
Code able to run `describe_recording` without further steps. The mutating
tier is gated on a test that the flag-off server rejects a mutating call with
a message naming the flag.

## Deferred / Reopen

- **`viewer_link` with a full URL.** Built both ways: `--viewer-url` on the
  server or `viewer_url` on the call gives a full URL; without either the
  fragment and query come back on their own.
- **`rezolus mcp install` and the skill.** Next PR.
- **`set_kpis`.** Mutating replace of `service_queries`; the annotate KPI
  path exists (`RezAnnotation::ext_json`, `annotate_parquet`), so it is a
  small addition when a client asks for it. Not built with the first three
  tools since nothing on the agent side produces a KPI set today.
- **Skills for other clients.** Add per client when someone asks; the install
  command's client list is the place.

## Cross-references

- [Viewer links that carry the whole view](2026-09-28-viewer-link-state.md)
- [Checks with verdicts](2026-09-28-checks-with-verdicts.md)
- [Events as ranges](2026-09-28-events-ranges-and-alignment.md)
