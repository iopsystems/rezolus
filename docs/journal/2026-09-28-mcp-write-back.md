# MCP write-back and tool tiers

- **Opened:** 2026-09-28
- **Status:** OPEN — design, nothing built.

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

- **`viewer_link` with a full URL.** Needs a way for the server to know the
  viewer's address; reopen when the fragment-only form proves insufficient.
- **A `run_checks` tool.** After checks land.
- **Skills for other clients.** Add per client when someone asks; the install
  command's client list is the place.

## Cross-references

- [Viewer links that carry the whole view](2026-09-28-viewer-link-state.md)
- [Checks with verdicts](2026-09-28-checks-with-verdicts.md)
- [Events as ranges](2026-09-28-events-ranges-and-alignment.md)
