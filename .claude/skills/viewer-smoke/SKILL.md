---
name: viewer-smoke
description: Run the end-to-end viewer smoke test (`tests/viewer_smoke.sh`). Spawns `rezolus view` in upload-only, file, A/B, and proxy modes, exercises the API endpoints, and verifies experiment attach/detach. Use after any change touching `src/viewer/` or before opening a viewer-related PR.
---

# Viewer smoke test

Runs the viewer in four configurations against checked-in demo
parquets and asserts the public API endpoints behave correctly.
Designed to be invoked from a PR/commit hook so behavior regressions
in `src/viewer/` show up before review.

## Steps

1. **Run the script:**

   ```bash
   bash tests/viewer_smoke.sh
   ```

2. **Interpret the result:**

   - **Exit 0** → all assertions passed; viewer behaves correctly across
     upload-only, file mode, A/B compare mode, and proxy mode, including
     experiment attach/detach via the HTTP API.
   - **Exit 1** → first failed assertion is printed, plus the failing
     viewer's startup log. Read both before guessing — the log usually
     says exactly what blew up (port collision, missing parquet, panic,
     etc.).

3. **If the script fails, do *not* paper over by editing the assertions.**
   The script is the contract. If a behavioral change is intentional
   (e.g. a renamed JSON field), update the assertion in the same commit
   that changes the behavior, and call it out in the PR description.

## What it covers

- `/api/v1/mode` — `loaded`, `compare_mode`, `url_loading` flags across
  the four startup configs
- `/api/v1/sections` — navigation list non-empty
- `/data/<section>.json` — at least one group in the lazy-section payload
- `/api/v1/query_range` — PromQL execution returns `status:"success"`
- `/api/v1/load_url` — 403 envelope when proxy disabled, `invalid_parquet`
  envelope when fetching non-parquet bytes from an allowed host
- `POST /api/v1/captures/experiment` — flips `compare_mode:true`
- `DELETE /api/v1/captures/experiment` — flips `compare_mode:false`
- Static assets — `/`, `/about`, `/lib/style.css` all return 200

## What it does *not* cover

- **Live mode** (`rezolus view <agent-url>`) — needs an actual rezolus
  agent listening, not reproducible without one.
- **Browser-rendered UI** — the script only asserts on backend payload
  shape. For the rendered page, use the `viewer-render` skill: it drives a
  headless browser and asserts on geometry, computed colours, wrap points
  and console errors.
- **Compare-mode UI rendering** — the experiment attach/detach assertion
  only checks the API flag flip; the rendered side-by-side view isn't
  exercised. Again `viewer-render`.

## Requirements

- `cargo` — builds the binary if it isn't already built
- `bash` 4+, `curl`, `jq` — assertions
- Ports 18500-18503 free
- Internet access to `httpbin.org` for the proxy assertion (skipped
  with a warning if unreachable)

## Triggering it from a hook

A typical wiring:

- **Per-PR (CI)** — add a job to `.github/workflows/cargo.yml` that runs
  `bash tests/viewer_smoke.sh` after `cargo build`.
- **Per-commit (local)** — `.git/hooks/pre-push` calling the same
  script. Slow (cold builds take ~60 s + 8 s of viewer warmup), so
  prefer the CI route unless the dev wants the early signal.
- **Per Claude Code session** — invoke this skill via `/viewer-smoke`
  or have the agent call it from a SessionStart / PostToolUse hook
  that watches `src/viewer/**`.
