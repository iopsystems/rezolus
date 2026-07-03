---
name: document-feature
description: Write or update the CLI `--help` text and README section for a new or changed rezolus feature (a subcommand, flag, mode, or behavior change), then prove the help is usable by dispatching a fresh subagent that has never seen the code. Use this whenever you add or change how rezolus is invoked — new `rezolus <subcommand>`, new flags/args on an existing mode, renamed options, or changed defaults — and any time the user says the help/README is thin, stale, confusing, or "an agent couldn't figure this out." Treats help text as an interface under test, not prose.
---

# Documenting a rezolus feature

Rezolus is increasingly driven by LLM agents, not just humans skimming a
terminal. For an agent, `--help` *is* the interface: it reads the help, decides
how to invoke the tool, and lives or dies by whether the text is unambiguous.
Thin help ("bare usage line, no examples, unexplained jargon") produces wrong
invocations that fail silently or, worse, do the wrong thing.

So treat help text like code under test. **Write down the intended usage first,
then prove the help conveys it to an agent that has never seen the codebase.**
That "prove it" step is the whole point of this skill — without it you're just
guessing that your prose is clear.

## When this applies

Any change to how rezolus is invoked: a new `rezolus <subcommand>`, new
flags/args on an existing mode, renamed options, changed defaults, or a behavior
shift that the current help now describes incorrectly. The seven modes are
agent, exporter, recorder (`record`), hindsight, viewer (`view`), mcp, and
parquet — each has a `command()` builder in its module (`src/<mode>/mod.rs` or
`src/parquet_tools/mod.rs`).

## The loop

### 1. Capture intent — write the "test" before the docs

Read the diff or the new code and figure out what actually changed. Then write
down **3–5 representative tasks** a user or agent would want to accomplish with
the feature, each paired with the **exact correct invocation**. For example:

```
Task: record from a Prometheus endpoint for 5 minutes, tagging the source as "llm-perf"
Cmd:  rezolus record --metadata source=llm-perf --duration 5m http://host:9090/metrics out.parquet
```

**Cover the command's distinct modes, not just the happy path.** If the command
has mutually-exclusive input modes, output formats, or a flag that changes the
shape of the run (e.g. `record`'s positional-URL vs `--endpoint` vs `--config`,
or `--separate`), give each its own task. This matters because the blind sims in
step 3 are your strongest signal, and a sim can only exercise a surface you wrote
a task for — if every task is single-endpoint, the sims stay silent on the
multi-endpoint help no matter how broken it is, and you're left relying on the
critic alone. List the modes first, then make sure your task set touches each.

Write these down **now, before touching any help text**. They are ground truth
and must not change during the revise loop — otherwise you're grading the help
against a moving target and the verification means nothing. Keep them in a
scratch file so the subagents and you refer to the same list.

Pull realistic tasks from the CLAUDE.md "Running Modes" block — it already
enumerates canonical invocations for every mode and is the best source of "what
do people actually run."

### 2. Write the docs

**CLI help** — edit the clap `command()` builder for the affected mode:
- `.about(...)` — one line, what the command is for.
- `.long_about(...)` — for anything non-trivial, include **inline usage
  examples**. `src/mcp/mod.rs` is the exemplar: it pairs a prose description
  with concrete example invocations. Copy that shape.
- per-arg `.help(...)` — say what the arg *is* and, when the value isn't
  obvious, give an example value (paths, PromQL snippets, `key=value` forms).

Rezolus uses clap's **builder** API, not derive — help lives in these method
calls, not in doc comments.

**README** — update the matching usage section so a human reader sees the same
story.

**CLAUDE.md sync** — if you added or changed an invocation, update the "Running
Modes" block too. It's effectively the canonical example source; letting it
drift from `--help` defeats the purpose.

### 3. Verify with subagents — two lenses, in parallel

Render the *actual* help an agent will see (not the Rust source strings):

```bash
cargo run --quiet -- <subcommand> --help
```

First render triggers a build — that's expected; warn nobody, just wait. If
you're rendering from a throwaway worktree (a good idea, so the working tree
stays clean while you iterate), point `CARGO_TARGET_DIR` at the main repo's
`target/` so the build reuses existing artifacts instead of compiling from cold.
Capture that exact output and feed it to two independent subagents, dispatched in
the same turn so they run concurrently:

**Lens A — blind user-simulation** (one subagent per intended task). Give it
*only* the rendered `--help` text plus one task in plain English. Instruct it
explicitly: rely solely on the provided help — no repo access, no web, no prior
rezolus knowledge — and return the command it would run plus a one-line
rationale. See `references/subagent-prompts.md` for the exact prompt. Then
compare its command to your ground truth **semantically**: right subcommand, all
required args present, correct flags/values — tolerant of arg ordering and
equivalent forms. A structural mismatch is a fail; a genuinely ambiguous case
(two defensible readings) is itself a finding — the help didn't disambiguate.

**Lens B — fresh-eyes critic** (one subagent). Give it the same rendered help
and ask for structured findings: ambiguous flags, missing examples, undefined
jargon, unclear when-to-use-this-vs-that. Prompt in
`references/subagent-prompts.md`.

Blind sims catch *insufficiency* (an agent can't get to the right command); the
critic catches *unclarity* the sims might pass by luck. You want both.

### 4. Auto-revise, capped at 3 rounds

If any blind task failed or the critic flagged a material gap, revise the help
(and README/CLAUDE.md) to address the **specific** finding — not a vague reword.
Re-render, re-run both lenses. Loop until every blind sim passes and the critic
has no material findings, or you hit **3 rounds**.

The cap exists because past ~3 rounds you're usually fighting a genuine design
ambiguity (two flags that really do overlap, a mode whose purpose is unclear)
that reprose won't fix — surface it to the user instead of grinding. Each round,
report what failed and what you changed, so the human can see the reasoning.

A reliable tell: the **same WHEN_TO_USE finding recurs** across rounds — e.g. the
critic keeps flagging that three different flags all set the same thing. That's
not a docs bug you can write your way out of; it's the CLI itself offering an
agent several ways to do one thing. Name it to the user as a design smell rather
than papering over it with another paragraph of prose.

### 5. Report

Summarize: the intended tasks, per-task pass/fail across rounds, the critic's
findings and how each was resolved, the diffs applied (help + README +
CLAUDE.md), and any residual ambiguity if you hit the cap without a clean pass.

## Notes

- **Keep ground truth immutable.** The single most common way to fool yourself
  here is to "fix" the expected command mid-loop to match what the subagent
  produced. Don't. If the intended usage was wrong, that's a code/design bug, not
  a docs bug — stop and raise it.
- **Blind means blind.** If a subagent's rationale references something not in
  the help text you handed it, it cheated (or you leaked context) — rerun it
  clean, otherwise the pass is meaningless.
- **This is a dev-time skill, not CI.** It needs model access and is
  nondeterministic; don't wire it into `cargo test`. Invoke it while writing the
  feature, before the PR.
