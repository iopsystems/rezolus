---
name: sync-viewer-symlinks
description: Ensure site/viewer/lib has correct symlinks to src/viewer/assets/lib
---

Synchronize symlinks in `site/viewer/lib/` so that the site viewer picks up
all shared modules from `src/viewer/assets/lib/`.

## Context

The site viewer (`site/viewer/`) shares most of its JavaScript and CSS with
the agent viewer (`src/viewer/assets/`). Shared files are kept as **symlinks**
in `site/viewer/lib/` pointing into `src/viewer/assets/lib/`. A small set of
site-specific files are standalone (not symlinked).

When new files are added to `src/viewer/assets/lib/`, the corresponding
symlink in `site/viewer/lib/` must be created manually. This skill detects
and fixes any missing symlinks.

## Standalone files (never symlinked)

Only these two top-level files in `site/viewer/lib/` are site-specific real
files (site keeps its own copy):

- `script.js` — site-specific entry point
- `viewer_api.js` — site-specific API transport layer (calls the in-browser
  WASM registry instead of the HTTP backend)

Everything else — **including `data.js`** — is a same-name symlink into
`src/viewer/assets/lib/`. (There is no `data_base.js`; that was an older
architecture. `dashboards.js` no longer exists.)

## Coverage can be per-file OR a directory symlink

Most of `site/viewer/lib/` is per-file symlinks, but some subtrees are covered
by a **directory symlink** — e.g. `site/viewer/lib/embed ->
../../../src/viewer/assets/lib/embed` — which serves every file underneath it.
So the invariant is **resolution, not per-file-symlink presence**: every shared
module must *resolve* at the same relative path under `site/viewer/lib/`.

A tool that looks only for a per-file symlink (or a `find` that doesn't descend
through a directory symlink) will **false-flag** files under a directory
symlink. Test with `[ -e "$link" ]` (follows all symlinks), never `[ -L ]`.
CAUTION: because `site/viewer/lib/embed` is a directory symlink into `src/`,
writing to `site/viewer/lib/embed/X` writes *through* it into `src/…/embed/X` —
never `mkdir`/`ln` inside a directory-symlinked path.

## Enforced in CI

`scripts/check-viewer-symlinks.sh` implements this (resolution-based) check and
runs on every PR via `.github/workflows/viewer-symlinks.yml`. Run it locally
before pushing a viewer change: `bash scripts/check-viewer-symlinks.sh`.

## Steps

1. **Scan** `src/viewer/assets/lib/` recursively for all `.js` and `.css` files

2. **For each source file**, determine the expected path in `site/viewer/lib/`:
   - Skip the standalone top-level files `script.js`, `viewer_api.js`
   - Everything else → same relative path, which must **resolve** (`[ -e ]`)
     via either a per-file symlink or a covering directory symlink

3. **Check** whether the expected symlink exists and points to the correct
   target. Compute the relative path from the symlink location back to the
   source file (e.g., `../../../src/viewer/assets/lib/charts/chart.js` for a
   file in `site/viewer/lib/charts/`).

4. **Create** any missing symlinks. Create parent directories if needed.
   Report each symlink created.

5. **Detect** any stale symlinks in `site/viewer/lib/` that point to
   non-existent source files, and report them (but don't delete without
   asking).

6. **Stage** newly created symlinks with `git add`.

7. **Report** a summary: how many symlinks checked, how many created, any
   stale links found.

## Pre-commit hook

A Claude Code hook at `.claude/settings.json` runs
`.claude/scripts/pre-commit-check.sh` before every `git commit`. It blocks
the commit if:

- Any shared viewer module fails to resolve under `site/viewer/lib/`
- Dashboard JSON is out of date with Rust definitions (only when
  `src/viewer/dashboard/` or `src/viewer/plot.rs` files are staged)

The hook **wiring** (`.claude/settings.json`) is per-checkout Claude Code
config, so the hook only fires for local Claude Code users — that is exactly
why the symlink check is *also* enforced in CI (`viewer-symlinks.yml` →
`scripts/check-viewer-symlinks.sh`), which is the binding gate for every PR.
To set up the local hook on a fresh checkout, create `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash(git commit*)",
        "hooks": [
          {
            "type": "command",
            "command": ".claude/scripts/pre-commit-check.sh",
            "timeout": 120
          }
        ]
      }
    ]
  }
}
```

And copy or recreate `.claude/scripts/pre-commit-check.sh` (see the
existing copy in this repo's working tree for reference).
