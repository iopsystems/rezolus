---
name: engineering-journal
description: Use when starting or picking up a non-trivial effort (feature, investigation, perf probe, refactor, migration) in a shared repo where teammates or future agents must coordinate or hand off; when a repo has no durable in-tree record of decisions and dead-ends; when you're about to drop a well-measured negative result; or when bootstrapping a journal from a repo's commit history.
---

# Engineering Journal

## Overview

An engineering journal is an **in-repo markdown record of efforts** — what you set out to do, the decision to proceed or not, what happened, and what was learned. It lives in the tree (`docs/journal/` or the repo's docs dir), lands on `main` via PR, and is grounded in code (real commits, files, specs).

**Core principle: an effort's *record* is a deliverable, landed alongside the code — not a closed issue, not notes in your head.** A well-measured dead-end is the most valuable entry: an unrecorded NO-GO is the one the team re-pays to rediscover.

## When to use

- Starting a non-trivial effort a teammate or future agent might need to understand or continue.
- Picking up someone's in-progress work — read their entry to continue.
- A repo with no decision/dead-end record — establish the journal as the convention.
- Bootstrapping from a repo's commit history (retrospective mode, below).

Not for trivial one-liners.

## Journal vs. issues (use both)

Issues/PRs are the *task* layer — discrete units, assignment, notifications. The journal is the *narrative/decision* layer — why, the dead-ends, current state, how to continue. It must be in-repo: versioned with the code, greppable, code-grounded, and readable by the next agent without leaving the tree. Link the journal to issues/PRs; don't let an issue be the only record of a non-trivial effort.

## The lifecycle (per effort)

1. **Pick & scope.** Gather requirements. Grep the journal + git log for prior art first — don't re-litigate a settled question.
2. **Open — land intent on `main`.** Write the entry: goal/hypothesis, requirements, GO/NO-GO criteria (number-gated where possible), plan. Commit via PR to `main` so the effort is visible *before* you build. This is the coordination move.
3. **Go / no-go.** Probe or price it cheaply before building. Record the verdict honestly.
4. **Implement & test.**
5. **Close out — in the implementing PR.** Update the entry with the outcome (shipped / NO-GO + numbers + mechanism) and update any docs the change affects. Landing the work closes the record in the same PR.

A NO-GO closes out the same way: land the negative result with its mechanism and a reopen condition ("revisit if new hardware / data / regime"). Merge negative probes; don't abandon them.

## Ground every claim in code

The journal's authority is that it traces to source: real commit SHAs, file paths, spec/note files, measured numbers — never invented figures. When you update or reconstruct an entry, re-verify against current code; **stale claims are the main failure mode.** If a detail isn't in the source, say so or omit it.

## Honest-ledger voice

Factual, not diaristic or triumphant. NO-GOs and falsifications are first-class, with their mechanism. Flag what you couldn't measure. Don't overclaim — the record is trusted only if it's honest about what didn't work and what's uncertain.

## Retrospective mode (bootstrap)

For a repo without a journal: cluster the commit history into thematic **arcs/campaigns**, write one grounded entry per arc from its commit range + design docs + notes (one drafter per arc parallelizes well), and add a series index. Same grounding and voice rules.

## Optional: publish as docs

The journal can feed a doc site in **whatever the repo already uses** (mdBook, another SSG, plain markdown, the repo's existing docs) — don't impose a toolchain. Journals stay source-of-truth; the site consumes them. A concrete worked example (private-by-construction mdBook with a build + link-check gate) and reusable scripts are in `publishing-example.md`.

## Related

Prioritizing and picking *which* effort, and maintaining the consolidated NO-GO ledger / reopen conditions, belong to a backlog/roadmap skill (if present). The journal records a single effort; the backlog orders them.

## Common rationalizations

| Rationalization | Reality |
|---|---|
| "No journal convention here; I won't invent one for one item." | The journal *is* the convention — establish it once. It compounds; every entry saves the next person from re-deriving. |
| "A GitHub issue is enough." | Issues aren't versioned with the code, aren't greppable in-repo, aren't code-grounded, and the next agent won't find them. The record belongs in the tree. |
| "I'll keep scratch notes in my head." | Then the reasoning is lost at handoff. The journal is where the reasoning lives. |
| "It's a dead end — nothing to record." | The dead-end is the highest-value entry. Record the mechanism and the reopen condition. |
| "I'll write it up after it ships." | Land the record in the same PR. "After" = never, or an unverified reconstruction. |
| "Close enough on the numbers." | Ground every figure in a source or omit it. Invented numbers destroy the record's authority. |

## Red flags — stop

- About to implement without landing intent for coordination.
- Recording an outcome you didn't verify against code.
- Dropping a negative result instead of landing it.
- Reaching for a GitHub issue as the *only* record of a non-trivial effort.
