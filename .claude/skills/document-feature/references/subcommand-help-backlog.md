# Handoff: bring the rest of `rezolus`'s CLI help up to the `record` bar

`rezolus record --help` was reworked to be agent-usable (PR #983) using the
`document-feature` skill. This is the backlog + playbook for doing the same to
every other subcommand. Do them one PR at a time; each is small and independent.

## How to do one

Invoke the **document-feature** skill (`.claude/skills/document-feature/SKILL.md`)
and point it at a subcommand. The loop in brief:

1. Read the subcommand's `command()` builder and its `config`/`run` code. Write
   down 3–5 task→command ground-truth pairs **covering the command's distinct
   modes**, before editing anything.
2. Add a `long_about` with worked examples (copy the shape of
   `src/recorder/mod.rs` / `src/mcp/mod.rs`), and give every arg a concrete value
   format. Update the README + CLAUDE.md "Running Modes" block to match.
3. Verify: render `cargo run --quiet -- <cmd> --help`, feed it to blind
   user-simulation agents (one per task) + one fresh-eyes critic. Revise up to 3
   rounds.

## Conventions that cost time on #983 — don't relearn them

- **Verify, never guess.** Read the actual parser/config before documenting a
  value format or default. On `record` this caught a wrong TOML key
  (`[[endpoints]]` not `[[endpoint]]`) and a wrong claim about `--duration` under
  command-wrapping. If you can't confirm it from code, don't write it.
- **Cover distinct modes in the ground-truth set.** Blind sims only exercise a
  surface you wrote a task for. A happy-path-only task set will pass while the
  multi-mode help is broken — the critic is your only backstop there.
- **Lead with canonical flags; mark deprecated ones.** `record` printed runtime
  deprecation notes for positional `URL`/`OUTPUT` while its help still led with
  them. Check `config.rs` for deprecation paths before writing.
- **Work in a throwaway worktree off `upstream/main`**, and set
  `CARGO_TARGET_DIR=<main-repo>/target` so the `--help` render reuses build
  artifacts instead of compiling cold.
- **Base PRs on `upstream/main`, not a feature branch.** Keep each diff to the
  one `command()` builder (+ README) so review is trivial.
- Recurring WHEN_TO_USE critic findings (several flags doing one thing) are a
  **design smell** — surface to the maintainer, don't paper over with prose.

## Targets (rough priority order)

### 1. `view` — highest value, most modes
`src/viewer/mod.rs`. Has decent per-arg help but no `long_about`/examples.
Modes to cover as separate tasks: parquet file, A/B compare (two files), live
agent (`view http://host:4241`), upload-only (no path). Plus `--listen`,
`--category`, the proxy flags (`--proxy-allow`/`--proxy-allow-any`), and
`--cache-size-mb`. Agents and humans both hit this one a lot.

### 2. `parquet` and its subcommands
`src/parquet_tools/mod.rs`. Subcommands: `metadata`, `annotate`, `combine`,
`filter`. All have `about` but zero examples. Each subcommand wants its own
`long_about` + example (esp. `annotate` with its many event/service-extension
flags, and `combine`'s multi-node/`--ab` semantics). Highest surface area.

### 3. `exporter` and `hindsight` — quick wins
`src/exporter/mod.rs`, `src/hindsight/mod.rs`. Both are thin: `about` + a single
config-file arg. Add a `long_about` that says what the mode is for, the scrape-
interval-must-match-Prometheus constraint (exporter), the ring-buffer/snapshot
model (hindsight), and point at the example config in `config/`. Small PRs.

### 4. Top-level `rezolus --help` + `agent`
`src/main.rs`. The root `long_about` is one bland sentence. Replace with a short
overview of the seven modes (agent, exporter, record, hindsight, view, mcp,
parquet) and when to reach for each — this is the first thing an agent reads.
The default no-subcommand `agent` mode (positional `CONFIG`) should be explained
here too.

### 5. `mcp` — lowest priority, already the best
`src/mcp/mod.rs`. Already has per-subcommand `about` and two `long_about`s. Fill
the remaining subcommands' `long_about`s and add a top-level examples block; it's
the most agent-facing surface, so worth polishing once the thinner ones are done.

## Done
- `record` — PR #983.
- `view` — `long_about` with worked examples for all four input modes (file, A/B,
  live, upload-only) + advanced-flag notes (`--proxy-*`, `--category`).
- `parquet` (+ `metadata`/`annotate`/`combine`/`filter`) — top-level `long_about`
  and per-subcommand `long_about`s with examples; `combine --ab` now spells out
  that `baseline=`/`experiment=` take a file's embedded source name, not filename.
- `exporter`, `hindsight` — `long_about` describing the mode, the config keys, the
  scrape-interval / ring-buffer + SIGHUP model, pointing at `config/`.
- top-level `rezolus` + `agent` — root `long_about` overviewing every mode
  (incl. `status`) and the default no-subcommand agent `CONFIG` form.
- `mcp` — top-level examples block + remaining subcommand `long_about`s
  (`describe-recording`, `describe-metrics`, `analyze-correlation`).

Verified with blind user-simulation agents (16 task→command pairs, all passed) and
fresh-eyes critics per surface; README + CLAUDE.md "Running Modes" synced.
