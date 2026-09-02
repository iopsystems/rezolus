# site/

The source for **rezolus.com**, deployed to GitHub Pages. Three things live
here, and they work quite differently:

| Path | What it is | Build step? |
| --- | --- | --- |
| `index.html` | The landing page | None — plain HTML |
| `docs/*.html` | The documentation pages | None — plain HTML |
| `viewer/` | The browser-only viewer app | Yes — WASM, see below |

## Preview it locally

Serve `site/` as the document root:

```bash
python3 -m http.server -d site 8000
# → http://localhost:8000
```

Then edit and reload. Hard-refresh (Ctrl/Cmd-Shift-R) after changing CSS —
the styles are inline in each page, so a cached `index.html` is a cached
stylesheet.

Two things that will otherwise look like bugs:

- **Serve it; don't open the file.** The pages use root-absolute paths such as
  `/favicon.svg`, which `file://` cannot resolve. Hence `-d site` rather than
  running the server from the repo root.
- **It needs network access.** Tailwind is pulled from `cdn.tailwindcss.com`
  and the typefaces from Google Fonts, both at page load. Offline, you get an
  unstyled page — check this first if the layout looks broken.

`/viewer/pkg/wasm_viewer.js` 404s until you build the viewer (below). That
affects only the viewer app, not the landing or docs pages.

## Editing the landing page

Everything is in `index.html`: markup, the Tailwind config, and an inline
`<style>` block. No toolchain, no partials.

- **Colors and fonts** are declared in the `tailwind.config` block in `<head>`
  (`primary` is `#0059ba`; Inter for text, Jost for headings, JetBrains Mono
  for the wordmark). Prefer those tokens over one-off hex values.
- **Dark mode** is `darkMode: "media"` — it follows the OS setting, and there
  is no toggle. Every colored element needs its `dark:` variant, or it will
  look wrong for half your readers.
- **Icons** are Material Symbols Outlined, written as
  `<span class="material-symbols-outlined" data-icon="name">name</span>`. The
  whole icon font is loaded, so any valid symbol name works.
- **The background** is triangular ("isometric") graph paper, drawn as a
  repeating **SVG tile** (`--paper-tile`) rather than as CSS gradients. This is
  deliberate: three `repeating-linear-gradient` families each measure phase from
  their own gradient line's start, which depends on the box size and the angle,
  so they land at arbitrary offsets and never share a vertex — the lines miss
  each other instead of meeting three-at-a-point the way graph paper does. A
  tile gets concurrency by construction. It sits on `body`; sections painted
  `bg-white dark:bg-slate-900` cover it, so the page reads as alternating
  textured and clean bands. `.paper-major` layers `--paper-tile-major`, the same
  tile at exactly 6× — six small triangles along each side of a major one — so
  its vertices land *on* minor vertices. Both are
  `background-attachment: fixed`, which is what keeps them in register while
  scrolling — change one and you must change the other. To resize the grid,
  scale the tile's `width`/`height`, its `viewBox`, and the matching
  `background-size` together, keeping the major tile at 6×.
- **Text blocks sit on their own surface** so a grid line never runs behind a
  line of type. `--surface-solid` is the page's own pale-blue ground at 88%
  rather than white, so panels read as cleared areas of the paper instead of
  cards pasted onto it. Three ways to apply it: `.card-surface` for a standalone
  block (it adds padding and a radius), `.paper-cards` on a grid to surface each
  child, and `.paper-fill` / `.surface-soft` for blocks that already carry their
  own padding and border. The hero is deliberately left bare — the type is heavy
  enough to hold its own over the pattern.
- **Use-case and component cards have a header band.** `.paper-cards` styles
  its grid children directly — the first child (icon plus title) gets the
  near-white `--band` and a hairline rule, the paragraph gets its own padding,
  and the card itself carries `overflow: hidden` so the band's corners follow
  the card radius. It is all CSS on the grid container, so adding a card means
  writing header-div-then-paragraph and nothing else. In dark mode `--band` is
  a lifted slate, not white.

## Editing the docs pages

Same deal — self-contained HTML per page. The one piece of automation is the
sidebar version label:

```bash
./scripts/sync-docs-version.sh
```

It rewrites the version in every `site/docs/*.html` to the latest **stable**
release tag (pre-release tags are ignored, since the docs should describe what
people can actually install). It is idempotent, and the `pr` skill runs it, so
you rarely need to run it by hand.

`docs/architecture.svg` is a **symlink** to the repo-root `docs/architecture.svg`
— the same diagram the project README embeds — so the two can never drift. It is
generated from `docs/architecture.dot`; after editing the graph, regenerate with:

```bash
dot -Tsvg docs/architecture.dot -o docs/architecture.svg
```

The page frames it on a white panel in both themes. That is deliberate: graphviz
draws it with pastel fills on white, so on a dark page it reads as a lit plate
rather than as a diagram that forgot to invert.

All six docs pages share one layout, and it should stay that way: a
`max-w-6xl` flex wrapper, a `w-56` sidebar, and
`<main class="flex-1 px-8 lg:px-16 py-12 min-w-0 max-w-3xl lg:max-w-4xl">`. Three
pages had drifted to a narrower `max-w-3xl`, which is visible the moment you
navigate between them. Copy the block from an existing page rather than
improvising a new one.

## The viewer subdirectory

`viewer/` is not hand-maintained content. It is the same frontend the
server-backed `rezolus view` serves, wired to a WASM backend so a recording can
be opened entirely in the browser.

- **`viewer/lib/` and `viewer/templates/` are symlinks** into
  `src/viewer/assets/lib/` and `config/templates/`. Edit the real files there,
  never the links. If a link is missing, the deployed viewer 404s silently —
  the `sync-viewer-symlinks` skill and a CI check exist because that has
  happened.
- **`viewer/pkg/` is generated and not checked in.** Build it with:

  ```bash
  ./crates/viewer/build.sh
  ```

  It needs `wasm-pack` and a clang that can target wasm32 (Apple's cannot;
  Homebrew LLVM can — the script picks it up automatically when `brew` is
  present).

## How it deploys

`.github/workflows/pages.yml`, on push to `main`, when any of `site/**`,
`src/viewer/assets/lib/**`, `crates/viewer/**`, `crates/dashboard/**`,
`config/templates/**`, `Cargo.toml`, `Cargo.lock` or the workflow itself
changes.

The job builds the WASM viewer, then runs `cp -rL site site-resolved` to
**resolve the symlinks into real files** before uploading — Pages will not
follow them. That is why a broken symlink shows up as a 404 on the live site
rather than as a build failure.
