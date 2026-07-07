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
2. **Open — land intent on `main`.** Write the entry: goal/hypothesis, requirements, GO/NO-GO criteria (number-gated where possible), plan. Commit via PR to `main` so the effort is visible *before* you build. This is the coordination move. If the entry names deferred/reopen items, mirror them into `docs/backlog.md` in the same commit (see "Keep the backlog in sync").
3. **Go / no-go.** Probe or price it cheaply before building. Record the verdict honestly.
4. **Implement & test.**
5. **Close out — in the implementing PR.** Update the entry with the outcome (shipped / NO-GO + numbers + mechanism) and update any docs the change affects. Reconcile `docs/backlog.md`: add any new deferred items this effort leaves behind, and **remove or mark done every backlog item this landing completes or deprecates** — in the same PR. Landing the work closes the record.

A NO-GO closes out the same way: land the negative result with its mechanism and a reopen condition ("revisit if new hardware / data / regime"). Merge negative probes; don't abandon them.

## Ground every claim in code

The journal's authority is that it traces to source: real commit SHAs, PR numbers, source-code paths (`src/…`, `crates/…`), measured numbers — never invented figures, and never a transient design/spec doc that may be deleted (absorb its content instead — see below). When you update or reconstruct an entry, re-verify against current code; **stale claims are the main failure mode.** If a detail isn't in the source, say so or omit it.

## Absorb the design doc — the entry is self-contained

An effort's design often starts life as a **separate** spec/plan/brainstorm doc
(from a planning skill, an ad-hoc `docs/design/` or scratch location). Do **not**
leave that doc beside the journal and link to it: parallel records drift, and a
scratch doc that later gets deleted turns the journal's links into dangling
references.

When you journal an effort, **lift the design doc's durable content — the goal,
the decisions and their rationale, the GO/NO-GO, the dead-ends — into the journal
entry itself, then remove the consumed spec/plan doc in the same PR.** The entry
*becomes* the design record (as this skill's own entries do). A useful check: after
writing, `grep` the entry for any path you are about to delete — if a fact only
lives behind such a reference, lift it inline; then the reference goes. The entry
must read completely on its own, citing only things that persist (SHAs, PRs,
code paths).

## Honest-ledger voice

Factual, not diaristic or triumphant. NO-GOs and falsifications are first-class, with their mechanism. Flag what you couldn't measure. Don't overclaim — the record is trusted only if it's honest about what didn't work and what's uncertain.

## Retrospective mode (bootstrap)

For a repo without a journal: cluster the commit history into thematic **arcs/campaigns**, write one grounded entry per arc from its commit range + design docs + notes (one drafter per arc parallelizes well), and add a series index. Same grounding and voice rules — **lift the design docs' key decisions into the entries and remove the consumed docs** (see "Absorb the design doc"); the entries, not the scratch docs, are the record.

## Keep the backlog in sync

`docs/backlog.md` is a **derived index** of the journal's deferred/reopen items —
the *ordering* layer over the entries. It is not a second source of truth: every
item traces back to the journal entry that owns its "why" and mechanism. Because
it is derived, it goes stale unless updated *with* the journal, so treat it as
part of every journal change:

- **Adding an entry** (open or retrospective) whose Deferred/Reopen/limitations
  section lists items → add those items to `docs/backlog.md`, each linking its
  source entry and carrying its reopen condition.
- **Updating an entry** — new deferred items, or a resolved one → mirror the
  change in the backlog (add / edit / drop).
- **Landing work that completes or deprecates a backlog item** → remove it (or
  mark it done with the PR that closed it) in the same PR. A backlog that still
  lists shipped work is worse than none — it sends people to re-do or re-litigate
  finished efforts.

Keep items grounded (link the entry, cite code paths / PRs), and mark state
(Open / Roadmap / By-design) rather than deleting the reasoning. If the repo has a
dedicated backlog/roadmap skill, defer ordering and prioritization to it; this
skill still keeps `docs/backlog.md` *consistent with the journal*.

## Optional: publish as docs

The journal can feed a doc site in **whatever the repo already uses** (mdBook, another SSG, plain markdown, the repo's existing docs) — don't impose a toolchain. Journals stay source-of-truth; the site consumes them. A concrete worked example (private-by-construction mdBook with a build + link-check gate) and reusable scripts are in `publishing-example.md`.

## Related

The journal records a single effort; `docs/backlog.md` (see "Keep the backlog in sync") is the consolidated index of their deferred/reopen items, kept in step with the journal by this skill. *Prioritizing* which effort to pick next is a separate concern — defer it to a dedicated backlog/roadmap skill if the repo has one.

## Common rationalizations

| Rationalization | Reality |
|---|---|
| "No journal convention here; I won't invent one for one item." | The journal *is* the convention — establish it once. It compounds; every entry saves the next person from re-deriving. |
| "A GitHub issue is enough." | Issues aren't versioned with the code, aren't greppable in-repo, aren't code-grounded, and the next agent won't find them. The record belongs in the tree. |
| "I'll keep scratch notes in my head." | Then the reasoning is lost at handoff. The journal is where the reasoning lives. |
| "It's a dead end — nothing to record." | The dead-end is the highest-value entry. Record the mechanism and the reopen condition. |
| "I'll write it up after it ships." | Land the record in the same PR. "After" = never, or an unverified reconstruction. |
| "Close enough on the numbers." | Ground every figure in a source or omit it. Invented numbers destroy the record's authority. |
| "The spec doc already says all this — I'll just link it." | A separate spec drifts from the code and dangles when the doc is deleted. Lift its decisions into the entry and remove the doc; the entry *is* the design record. |

## Red flags — stop

- About to implement without landing intent for coordination.
- Recording an outcome you didn't verify against code.
- Dropping a negative result instead of landing it.
- Reaching for a GitHub issue as the *only* record of a non-trivial effort.
- Leaving a separate spec/plan/scratch doc beside the entry and linking to it, instead of lifting its content in and removing it — especially a doc slated for deletion (the link will dangle).
- Landing work that finishes or deprecates a backlog item without removing it from `docs/backlog.md`, or adding a journal entry's deferred items without mirroring them into the backlog.
