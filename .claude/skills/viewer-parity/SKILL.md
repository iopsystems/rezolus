---
name: viewer-parity
description: Use when adding or changing what the rezolus viewer exposes — an /api/v1 endpoint, a dashboard section, the metric catalog, metadata/description derivation, or the frontend data those depend on — or when editing src/viewer/ or crates/viewer/, or reviewing a viewer change. The viewer ships as two separate Rust backends (server + WASM) behind one shared frontend; changing one silently diverges them.
---

# Viewer parity (server ↔ WASM)

## Overview

`rezolus view` ships as **two backends behind one shared frontend**:

- **Server** — the `rezolus` binary, `src/viewer/*.rs` (axum HTTP: `routes.rs`,
  `metadata.rs`, `state.rs`, `actions.rs`, `source_kind.rs`, …).
- **WASM** — the separate `crates/viewer` crate, `crates/viewer/src/lib.rs`,
  which loads a parquet in-browser for the static GitHub-Pages site.
- **Shared frontend** — most of `src/viewer/assets/lib/**` is **symlinked**
  file-for-file into `site/viewer/lib/`, so both backends serve the **same**
  JS/CSS. **Two files are NOT symlinks** — `viewer_api.js` and `script.js` are
  **separately maintained copies** in `site/viewer/lib/`. `viewer_api.js` is the
  backend-adapter seam (server does HTTP `fetch`; WASM calls into the in-browser
  registry), so it *must* differ — which means a change to the server's
  `viewer_api.js` does **not** propagate to the WASM shell. Editing either →
  mirror the other copy.
  - **A new shared module under `src/viewer/assets/lib/` needs a resolving
    entry under `site/viewer/lib/`** (a per-file symlink, or coverage by a
    parent directory symlink like `site/viewer/lib/embed`) — or the deployed
    static viewer 404s on the import and **fails to load entirely**. This is
    now enforced in CI (`viewer-symlinks.yml` → `scripts/check-viewer-symlinks.sh`);
    run it locally before pushing a viewer change. (A missing `charts/boxplot.js`
    symlink shipped exactly this outage before the guard existed.)

`crates/viewer` **cannot depend on the `rezolus` binary crate** (separate wasm32
workspace), so any backend logic both need must live in the shared **`dashboard`**
crate or be **duplicated** in `lib.rs`. That structural gap is why viewer behavior
gets added to one side and silently missing from the other.

**Core principle: a change to what the viewer *derives from a loaded recording*
is a change to BOTH backends.** Shipping it on one is a parity regression, not a
smaller scope. (Real instance: per-source classification was added to the server's
`metadata.rs` and never mirrored in `lib.rs`, so simple-capture parquets showed no
`source:` section in the WASM viewer — see
[per-source descriptions](../../../docs/journal/2026-07-04-per-source-descriptions.md)
and the simple-capture entry.)

## The one legitimate exception

Something is **server-only** *only* when the capability structurally can't exist
in the browser: talking to a **live agent** (`/connect`), **proxy-fetching** a URL
(`/load_url`), server **reset**, upload plumbing. WASM operates on one
already-loaded parquet. Everything that is a **pure derivation from the loaded
recording** — sections, metric catalog, metadata, systeminfo, descriptions,
PromQL — exists on both. "It's just the server endpoint" is not an exception; it's
the regression.

## Server route ↔ WASM method map

The shared `viewer_api.js` calls `/api/v1/<x>` via `backendRequest`; the server
answers with an axum handler, WASM answers with a `lib.rs` method routed through a
shim. A new frontend-facing endpoint needs an entry on **both** sides or WASM 404s.

| Frontend calls | Server (`src/viewer/routes.rs`) | WASM (`crates/viewer/src/lib.rs`) |
|---|---|---|
| `/api/v1/sections`, `/data/<s>.json` | `sections_handler`, `data` | `get_sections`, `get_section` |
| `/api/v1/metrics` | `metrics_handler` | `metrics` |
| `/api/v1/systeminfo` | `systeminfo_handler` | `systeminfo` |
| `/api/v1/file_metadata` | `file_metadata_handler` | `file_metadata_json` |
| `/api/v1/metadata` | `metadata` | `metadata` |
| `/api/v1/selection` | `selection_handler` | `selection` |
| `/api/v1/query`, `/query_range` | `instant_query`, `range_query` | `query`, `query_range` |
| `/api/v1/save*` | `actions::save*` | `save_with_selection` |
| dashboard build (not an endpoint) | `metadata::regenerate_dashboards` → `classify_sources` | `init_templates` / `regenerate_combined` |
| `/api/v1/connect`, `/load_url`, `/reset` | server handlers | **none — server-only by nature** |

## How to keep them in parity

1. **Put the logic in `dashboard`, call it from both shells.** The endpoint
   handler and the WASM method are different *shells* (axum extractors vs
   `#[wasm_bindgen]`), but the *logic* should be one function in the `dashboard`
   crate that both call — like `metric_catalog::assemble_catalog` and
   `metric_catalog::resolve_descriptions`. Byte-identical output falls out for
   free. Do NOT copy logic into `lib.rs` when it can be shared.
2. **When a shell must differ, change both shells in the same PR.** Adding a route
   handler? Add the matching WASM method. Adding classification/section logic in
   `metadata.rs`? Mirror it in `init_templates`.
3. **Most frontend changes hit both automatically** (symlinked assets) — but only
   if the endpoint they call exists on both backends. Verify the WASM side answers
   it. Exception: `viewer_api.js` and `script.js` are per-shell copies, not
   symlinks — a change to one must be mirrored into the other.
4. **Duplicated files are a hazard.** Known hand-mirrored pairs: `report_save.rs`
   (`src/viewer/` **and** `crates/viewer/src/`), and the frontend `viewer_api.js` /
   `script.js` (`src/viewer/assets/lib/` **and** `site/viewer/lib/`). An edit to
   one copy must be mirrored to the other. Prefer folding backend duplication into
   `dashboard`; the `viewer_api.js` split is intrinsic (the adapter seam differs).

## Tests — so one side can't be silently ignored

Compilation parity is necessary but **not sufficient** — a WASM build can be green
while the WASM viewer produces different (or empty) output.

- **Build gate (necessary):** `cargo check -p viewer --target wasm32-unknown-unknown`
  and `./crates/viewer/build.sh`. Catches "forgot to compile the WASM side",
  never "forgot to implement the behavior".
- **Behavioral parity (the real gate):** for a set of fixture parquets (at least
  one Rezolus recording, one service/combined file, and one non-Rezolus *simple
  capture* like `hub.heartbeat.parquet`), assert the **server endpoint JSON and the
  WASM method JSON are byte-identical** for the same input — `sections`, `metrics`,
  `systeminfo`, `file_metadata`, `metadata`. A Rust test can construct
  `crates/viewer`'s `Viewer` from the fixture bytes and diff its `metrics()` /
  `get_sections()` output against the server path's output for the same fixture.
  When you add a derivation, add its fixture to this set — a simple-capture fixture
  is what would have caught the missing WASM `source:` section.
- **Smoke coverage is server-only today.** `tests/viewer_smoke.sh` exercises only
  the axum server (see the `viewer-smoke` skill). Adding a curl+jq check there does
  **not** cover WASM. Parity needs the WASM-side check above.

## Which side am I forgetting? (pre-PR checklist)

- Did I change what the viewer derives from a recording (sections/catalog/metadata/
  descriptions/query)? → it belongs on **both** backends.
- Is my new logic in `dashboard` (shared), or did I write it in `src/viewer/` only?
  If server-only, does WASM need it? (Almost always yes for derivations.)
- New `/api/v1/*` route the frontend calls? → matching `lib.rs` method added.
- Did I add a fixture-diff parity assertion, or only a server-side smoke check?
- Edited `report_save.rs`, `viewer_api.js`, or `script.js`? → mirror the other copy
  (these are hand-maintained duplicates, not symlinks).

## Rationalizations — STOP

| Excuse | Reality |
|---|---|
| "The task said the server endpoint, so WASM is out of scope." | A derivation feature on one backend is a parity regression. Scope is *the behavior*, which lives on both. |
| "I'll do the WASM side as a separate follow-up." | Follow-ups for parity don't happen; the WASM viewer ships broken meanwhile. Same PR. |
| "`cargo check -p viewer` passes, so WASM is fine." | Compilation ≠ behavior. An empty/wrong WASM result compiles clean. |
| "The frontend is shared, so it just works in both." | Only if the endpoint it calls exists on both. Shared frontend hitting a WASM-missing endpoint = silent 404. |
| "It's a small addition." | Small divergences are the ones that ship — no reviewer notices one missing method. |
