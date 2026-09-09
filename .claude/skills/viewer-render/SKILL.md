---
name: viewer-render
description: Render the running viewer in a real headless browser and assert on what it drew — element geometry, computed colours, wrap points, truncated labels, console errors. Use when a change moves pixels (navbar/badge/chart layout, CSS, responsive behavior, theme), when a PR says it "needs a visual check", or when reviewing frontend work under src/viewer/assets/. Covers the gap viewer-smoke names: it asserts payload shape, this asserts rendering.
---

# Rendering the viewer for real

## Why this exists

Three layers of viewer testing already exist and none of them can see the page:

- `tests/*.test.mjs` — pure frontend logic, no DOM by design (CLAUDE.md: "no
  bundler, no jsdom").
- `tests/viewer_smoke.sh` — API envelopes. Its own "what it does not cover"
  section says *"Browser-rendered UI … visual regressions need eyeball
  verification."*
- CI — compiles both backends. Compilation is not rendering.

So a change can be green everywhere and still ship a label at the disabled
contrast tier, a chip strip that wraps out of the navbar, or a mithril keyed
fragment that warns on every redraw. This renders the page in Chrome and
reports what actually got drawn.

## Two traps, both of which bite on the first run

**1. Set `REZOLUS_NO_OPEN=1`.** A second after binding, `rezolus view` opens the
dashboard in the user's real browser (`src/viewer/mod.rs`). It is an environment
variable, not a CLI flag, so it is easy to miss — and a headless session that
starts seven viewers across a review silently opens seven tabs on someone's
desktop. Always set it.

**2. Build with `--features developer-mode`.** The binary embeds
`src/viewer/assets/` at *compile* time. A normal `cargo build` bakes in whatever
the assets were when it last ran, so you screenshot the old frontend, see your
change missing — or worse, see something plausible and conclude it works.

```bash
cargo build --bin rezolus --features developer-mode          # assets from disk
REZOLUS_NO_OPEN=1 target/debug/rezolus view --listen 127.0.0.1:4299 /tmp/x.rez &
```

If the render disagrees with the source you are reading, it is trap 2.

## Setup

`puppeteer-core` plus a browser already on the machine. **Do not add either to
the repo** — it deliberately carries no JS toolchain, and `puppeteer` (without
`-core`) downloads ~150 MB of Chromium you do not need.

```bash
mkdir -p /tmp/viewer-render && (cd /tmp/viewer-render && npm i puppeteer-core)
export PUPPETEER_CORE=/tmp/viewer-render/node_modules/puppeteer-core
export CHROME_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

`CHROME_PATH` is optional where Chrome sits in a standard location; the script
probes the usual macOS and Linux paths.

Plain `chrome --headless --screenshot=out.png <url>` **hangs** on this page —
the dashboard never reaches the quiescent state that flag waits for. Use the
script, which waits on `networkidle2` plus a selector.

## Use it

```bash
# 1. Assets from disk, and no browser tab on the user's desktop.
cargo build --bin rezolus --features developer-mode
REZOLUS_NO_OPEN=1 target/debug/rezolus view --listen 127.0.0.1:4299 /tmp/three.rez &

# 2. Render and inspect.
node scripts/viewer_render.mjs \
  --url http://127.0.0.1:4299 \
  --selector '.compare-badge' \
  --children '.compare-badge-label' \
  --out /tmp/badge.png
```

Stdout is a JSON block: tag, classes, computed colours, box geometry, per-child
text/title/colour/truncation, `rowTops`, `wrapped`, `consoleProblems`,
`failedRequests`. Two PNGs land beside `--out`: the element, and the top band of
the page for layout context.

`--children` matters more than it looks. It defaults to *direct* children, and
in this UI the node carrying the text, the tooltip and the truncation is usually
a grandchild — a label inside a chip. Point it at that node or the interesting
fields all read `null`.

## Read the JSON, not just the picture

The screenshot is for the human in the PR. The JSON is what you assert on:

- **`wrapped` / `rowTops`** — distinct child row tops mean the content broke
  onto more than one line. Sweep `--width 1440 1280 1100 1000 820` to find the
  point where it happens; one screenshot at one width hides this entirely.
- **`children[].truncated`** — `scrollWidth` past the painted width means an
  ellipsis is eating text. If that child has no `title`, the text is
  **unrecoverable** by the user. Truncation plus a missing tooltip is a finding.
- **`children[].color`** — resolve the actual computed colour. `--fg-muted` is
  the *disabled* tier (`#484f58` on dark); `--fg-secondary` (`#8b949e`) is
  "dimmer but meant to be read". Picking the wrong one looks fine in the CSS and
  wrong on screen.
- **`consoleProblems`** — mithril keyed-fragment violations, exceptions in a
  redraw. Silent in every other test layer.
- **`failedRequests`** — a 404 on a newly added shared module means a missing
  `site/viewer/lib` symlink (see the `viewer-parity` skill), which breaks the
  static viewer completely.

## Check both themes

Colours are CSS custom properties overridden under `[data-theme="light"]`, and
`--fg-muted` inverts between them. `--theme light` / `--theme dark` sets the
attribute directly, so one run per theme covers both.

## Where the script lives, and why not `tests/`

`scripts/viewer_render.mjs`. CI runs `node --test tests/*.mjs`, and that glob
takes **every** `.mjs` in the directory, not just `*.test.mjs` — a tool parked
there gets executed as a test and fails the suite.

## Fixtures

`rez`'s fixture writer caps at four recordings:

```bash
cargo run -p rez --features test-support --example write_rez_fixture -- /tmp/four.rez 4
```

For more arms — worth doing, since layout problems only appear at scale —
combine archives:

```bash
target/debug/rezolus recording combine /tmp/four.rez /tmp/three.rez -o /tmp/seven.rez
```

## Known noise

A bare fixture has no systeminfo, so `/api/v1/systeminfo` and
`/api/v1/selection` 404 and surface in `failedRequests`. Not your change.

## What this still does not cover

- **The WASM/static viewer.** This drives `rezolus view` (the axum server). The
  frontend is symlinked and therefore identical, but the backend answering
  `/api/v1/*` is not — see the `viewer-parity` skill. To render that side you
  need `./crates/viewer/build.sh` and a static server over `site/`.
- **Interaction.** Hover, click, drag, dropdown open/close. Puppeteer can do all
  of it; the script just does not, yet.
- **Pixel regression.** No baseline images are stored, deliberately — they rot.
  The JSON facts are the durable assertion.
