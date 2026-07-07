# `document-feature` skill — agent-verified CLI help

- **Opened:** 2026-07-02
- **Status:** SHIPPED — merged (see PRs)
- **PRs:** #985 (prereq: lockfile/audit), #983 (first use: `record`), #984 (the
  skill), #986 (backlog + playbook), #987 (all remaining subcommands), #988
  (retire cleared backlog)

## Problem

As LLM agents increasingly drive CLIs, `--help` is no longer written only for
humans — it is the interface an agent reads to decide how to invoke the tool.
Rezolus's help output was thin: bare usage lines, no examples, unexplained
jargon. There was no repeatable way to write help text and *prove* it works
before shipping.

## Deliverable

A rezolus project skill at `.claude/skills/document-feature/` (`SKILL.md` +
`references/subagent-prompts.md` + `evals/trigger-evals.json`). Rezolus-specific
for now — it hardcodes the seven operating modes, the clap builder API, and
`src/mcp/mod.rs` as the help exemplar — but the subagent verification loop is
generic enough to factor out later.

## The methodology

The design principle is TDD applied to docs: **define intended usage first, then
prove the help conveys it** to an agent that has never seen the codebase. Moving
target is the enemy — ground truth must be written down and locked before any
help text is edited.

### 1. Capture intent (the "test")

Before touching any Rust source, write **3–5 representative tasks** — each a
plain-English description paired with the exact correct invocation. Key
discipline: **cover the command's distinct modes**, not just the happy path. If a
command has mutually-exclusive input modes or a flag that changes the shape of the
run, give each its own task. A blind sim can only exercise a surface it has a task
for; a task set that only exercises single-endpoint record misses the
multi-endpoint path entirely.

### 2. Write the docs

Edit the clap `command()` builder (`.about`, `.long_about` with inline examples,
per-arg `.help`), update the README section, and sync the CLAUDE.md "Running
Modes" block. The skill has a hard rule: **don't document a guess** — read the
actual parser before writing any claim about value formats, defaults, or config
keys. On `record` this rule caught a wrong TOML key (`[[endpoints]]` not
`[[endpoint]]`) and a wrong claim about `--duration` behavior under
command-wrapping.

### 3. Verify with two subagent lenses, in parallel

Render *actual* help (`cargo run --quiet -- <cmd> --help`) and feed it to two
independent subagents dispatched in the same turn:

- **Blind user-simulation (Lens A):** one subagent per intended task, given only
  the rendered `--help` plus a plain-English task. Prompt explicitly forbids repo
  access, web search, and prior rezolus knowledge — "rely ONLY on this help text."
  The subagent returns `COMMAND: <line>` + `WHY: <one sentence citing the help>`.
  The WHY is how you detect cheating: if it cites something not in the help text
  you handed it, rerun clean. Grading is semantic: right subcommand, all required
  args present, correct flags — tolerant of arg order and equivalent forms.
- **Fresh-eyes critic (Lens B):** one subagent, same rendered help, returning
  structured findings in four categories: AMBIGUOUS, MISSING_EXAMPLE, JARGON,
  WHEN_TO_USE. Sims catch *insufficiency*; the critic catches *unclarity* the sims
  might pass by luck.

Both prompts are verbatim in `references/subagent-prompts.md`.

### 4. Auto-revise, capped at 3 rounds

Fail or material finding → revise the specific text, re-render, re-run both
lenses. The cap exists because past ~3 rounds you're usually fighting a genuine
CLI design smell (two flags that genuinely overlap) that reprose won't fix.
Reliable tell: **the same WHEN_TO_USE finding recurs** — that's the CLI offering
an agent multiple paths to the same thing, not a docs gap.

### 5. Cross-platform safe, dev-time only

This is an on-demand skill, not CI. It needs model access and is nondeterministic.
The skill notes explicitly: invoke while writing the feature, before the PR; don't
wire it into `cargo test`.

## Prerequisite: #985

CI was blocked by two quick-xml advisories (RUSTSEC-2026-0194/0195) via a
transitive `plist` dependency pinned to quick-xml ^0.39 (fix needs >=0.41.0 but
the plist release hadn't shipped). Resolution: `cargo update` + `.cargo/audit.toml`
with explicit ignores. Note: `.cargo/*` (not `.cargo`) in .gitignore is what made
`!.cargo/audit.toml` take effect — an entry I'd have to look up every time without
this journal.

## First use: `rezolus record --help` (#983)

`record` was chosen as the first target because it has the most distinct modes:
Rezolus-agent URL, Prometheus URL, `--format raw`, `--metadata`, `--duration`,
`--interval`, the multi-endpoint config file, and the `--` command-wrapping form.
The skill produced expanded `long_about` with inline examples for each mode. Blind
lens hit **5/5** across all modes including the `-- <command>` wrapping. Merged
before the skill itself (#984) because the skill PR was still under review.

## Backlog and playbook: #986

#983 made clear the remaining subcommands needed the same treatment. Rather than
filing seven follow-up issues, the skill's `references/` directory got a
`subcommand-help-backlog.md` with priority order (view → parquet → exporter →
hindsight → top-level + agent → mcp) and per-command notes on what was thin. This
kept the work grounded in the skill's directory.

## Main applied the skill at scale: #987 and #988

After #984–#986 merged, the `document-feature` skill was used to run the full
backlog — every remaining subcommand (`view`, `parquet` + its four sub-subcommands,
`exporter`, `hindsight`, `agent`, top-level overview) — producing #987 in a single
PR. #988 then retired the now-cleared `subcommand-help-backlog.md` from
`references/` since it had served its purpose.

## Deferred: trigger optimizer

`skill-creator`'s `run_loop.py` description optimizer needs `ANTHROPIC_API_KEY`
and the `anthropic` Python SDK installed. The `claude` CLI auth does not expose the
key, so the optimizer couldn't run in this env. The 20-query trigger eval set is
bundled in `evals/trigger-evals.json` ready for when the key is available.

## Learnings

- **The "blind means blind" discipline is load-bearing.** If a subagent's rationale
  cites anything beyond the help text it was handed, the pass is meaningless — the
  WHY field exists specifically to catch this.
- **Covering modes > covering tasks.** Writing five tasks that all exercise the same
  code path gives a false sense of coverage. Enumerate modes first, then task-map.
- **The 3-round cap has a diagnostic corollary.** Recurring WHEN_TO_USE is a design
  signal, not a docs failure. Naming that distinction to the user is more useful
  than a fourth round.
- **Dev-time verification with the existing toolchain.** No new infra — just
  `cargo run --quiet` and the agent dispatch that's available in every session.
