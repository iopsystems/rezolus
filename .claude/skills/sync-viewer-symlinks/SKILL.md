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

These files in `site/viewer/lib/` are site-specific and must NOT be replaced
with symlinks:

- `data.js` — site-specific wrapper that imports shared logic from `data_base.js`
- `script.js` — site-specific entry point
- `dashboards.js` — site-specific dashboard definitions
- `viewer_api.js` — site-specific API transport layer

## Special mapping

- `src/viewer/assets/lib/data.js` is symlinked as `site/viewer/lib/data_base.js`
  (different name, because site has its own `data.js` wrapper)

## Steps

1. **Scan** `src/viewer/assets/lib/` recursively for all `.js` and `.css` files

2. **For each source file**, determine the expected symlink path in
   `site/viewer/lib/` using these rules:
   - Skip files that have standalone site-specific versions:
     `script.js`, `viewer_api.js` (top-level only)
   - Map `data.js` (top-level) → `data_base.js` symlink
   - Everything else → same relative path

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

- Any expected symlinks are missing in `site/viewer/lib/`
- Dashboard JSON is out of date with Rust definitions (only when
  `src/viewer/dashboard/` or `src/viewer/plot.rs` files are staged)

Both files are git-ignored (`.claude/*` excluding skills). To set up the
hook on a fresh checkout, create `.claude/settings.json`:

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
