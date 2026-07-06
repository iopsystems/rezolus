# Publishing a journal as docs — worked example

Optional. The journal stands on its own as in-repo markdown; this is one proven
way to render it (plus curated content) as a browsable site. Use whatever the
repo already uses — this example is mdBook, but the *pattern* is what matters,
not the tool.

## The pattern

1. **Journals stay source-of-truth** in `docs/journal/`. The site *consumes*
   them — never hand-edit rendered copies.
2. A **sync step** copies the journals into the site's source dir at build time
   (so their cross-links resolve) rather than duplicating them in git.
3. **Build + gate**: render the site, then run a link check (and, if you use
   diagrams, confirm they mounted). A broken internal link fails the build.
4. **Ship privately by default** for a private repo (see the privacy note) —
   build in CI and upload the site as a downloadable artifact, no public URL.

## Privacy gotcha (private repos)

GitHub Pages has no access control on personal accounts: a Pages site from a
personal private repo is either unavailable (Free) or fully public (Pro).
Access-controlled Pages needs Enterprise Cloud + an org. So for a private repo,
default to **build-in-CI + upload-artifact** (downloadable only by repo
members); leave a one-job path to public Pages for if/when the content goes
public.

## `scripts/build-docs-site.sh` (sync + build + link-check)

```bash
#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
journal_src="$root/docs/journal"
history_dst="$root/docs-site/src/history"   # gitignore this dir's synced copies

mkdir -p "$history_dst"
rm -f "$history_dst"/*.md
cp "$journal_src"/*.md "$history_dst/"       # README/index handled separately if needed

mdbook build "$root/docs-site"
python3 "$root/scripts/check-docs-links.py" "$root/docs-site/src"
echo "Built docs site -> $root/docs-site/book"
```

Gitignore the synced copies (`/src/history/*.md`) and the build output
(`/book`) in the site dir, so there is a single source of truth.

## `scripts/check-docs-links.py` (internal link check)

Validates that every inline relative `.md` link resolves. Preferred over a full
link-checker backend when the source contains prose that a strict checker
misreads (e.g. bracketed shorthand that isn't a link).

```python
#!/usr/bin/env python3
"""Fail if any inline relative Markdown link in <src> points at a missing file."""
import re, sys, pathlib
link_re = re.compile(r"\]\(([^)]+)\)")
src = pathlib.Path(sys.argv[1]).resolve()
errors, pages = [], sorted(src.rglob("*.md"))
for md in pages:
    for m in link_re.finditer(md.read_text(encoding="utf-8")):
        t = m.group(1).strip()
        if t.startswith(("http://", "https://", "mailto:", "#")):
            continue
        path = t.split("#", 1)[0]
        if path.endswith(".md") and not (md.parent / path).resolve().is_file():
            errors.append(f"{md.relative_to(src)}: broken link -> {t}")
if errors:
    print("Broken internal links:", *("  " + e for e in errors), sep="\n", file=sys.stderr)
    sys.exit(1)
print(f"Link check OK ({len(pages)} pages scanned).")
```

## CI (private artifact) — `.github/workflows/docs.yml`

```yaml
name: docs-site
on:
  push: { branches: [main], paths: ['docs-site/**', 'docs/journal/**', 'scripts/build-docs-site.sh'] }
  pull_request: { paths: ['docs-site/**', 'docs/journal/**', 'scripts/build-docs-site.sh'] }
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install mdBook
        run: curl -sSL "https://github.com/rust-lang/mdBook/releases/download/v0.4.52/mdbook-v0.4.52-x86_64-unknown-linux-gnu.tar.gz" | tar -xz -C /usr/local/bin
      - name: Build
        run: bash scripts/build-docs-site.sh
      - name: Upload site (private; repo members only)
        uses: actions/upload-artifact@v4
        with: { name: docs-site, path: docs-site/book, if-no-files-found: error }
# To go public later: add a deploy-pages job gated on main (configure-pages +
# upload-pages-artifact path: docs-site/book + deploy-pages), enable Pages.
```

## Diagrams (if used)

Text-based diagrams (e.g. Mermaid via `mdbook-mermaid`, bundled locally so a
private/offline site still renders) are diffable and reviewable. A syntax error
renders a *blank* diagram rather than failing the build — so the gate must also
**audit that diagrams mounted**: grep the built HTML for the render marker
(`class="mermaid"`) present and the unrendered fence (`language-mermaid`)
absent.

## Content pages, if you add them

Curated pages (overview, guide, architecture, comparisons) follow the same
discipline as journal entries: **grounded in code** (verify CLI flags, config
fields, and numbers against source, not memory), honest (cite figures or omit
them; don't overclaim), and reviewed against source before landing. One
drafting subagent per page/section parallelizes well; review each against its
sources before commit.
