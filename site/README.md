# site/

The source for **rezolus.com** (`index.html` + `docs/`), plus the browser-only
viewer under `viewer/`. Deployed to GitHub Pages on push to `main`.

## Testing it locally

Serve `site/` as the document root, then edit and reload:

```bash
python3 -m http.server -d site 8000
# → http://localhost:8000
```

Three things that will otherwise look like bugs:

- **Serve it; don't open the file.** The pages use root-absolute paths such as
  `/favicon.svg`, which `file://` cannot resolve. Hence `-d site` rather than
  running the server from the repo root.
- **It needs network access.** Tailwind comes from `cdn.tailwindcss.com` and the
  typefaces from Google Fonts, both at page load. Offline you get an unstyled
  page — check this first if the layout looks broken.
- **Hard-refresh after CSS changes** (Ctrl/Cmd-Shift-R). The styles are inline
  in each page, so a cached `index.html` is a cached stylesheet.

## The viewer needs building first

The landing and docs pages have no build step. `/viewer/` does:

```bash
./crates/viewer/build.sh     # writes site/viewer/pkg/, which is not checked in
```

Until you run it, `/viewer/pkg/wasm_viewer.js` 404s and the viewer will not
load; the landing and docs pages are unaffected. The build needs `wasm-pack`
and a clang that can target wasm32 (Apple's cannot; Homebrew LLVM can, and the
script finds it automatically).

`viewer/lib/` and `viewer/templates/` are symlinks into `src/viewer/assets/lib/`
and `config/templates/` — edit the real files there, never the links. A missing
link 404s silently in the deployed viewer, which is what the
`sync-viewer-symlinks` skill and its CI check exist to catch.
