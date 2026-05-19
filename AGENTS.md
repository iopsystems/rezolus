# AGENTS.md

Workflows for AI agents (Claude Code, etc.) operating on this repo.

## Screenshot-identity check vs. pre-change baseline

When refactoring the viewer's chart pipeline, dashboard generators, or
SQL emitters, verify that visible output across every sidebar route
matches the pre-change baseline. The chromium smoke harness is
deterministic (re-runs on the same branch produce byte-identical
PNGs), so pixel-diff is a reliable signal of behavior change.

### Workflow

1. **Create a worktree on the baseline commit.** Pick the commit
   you're changing _from_ — typically `origin/main` or the branch
   point your work diverged at.

   ```bash
   git worktree add /tmp/preplan <baseline-commit>
   # If metriken-query-sql is a sibling-path workspace dep, the
   # worktree's `../metriken/...` path resolves wrongly; symlink:
   ln -sfn /work/metriken /tmp/metriken
   ```

2. **Build both binaries (release).** Smoke is too slow on debug.

   ```bash
   cargo build --release --bin rezolus
   (cd /tmp/preplan && CARGO_TARGET_DIR=/tmp/preplan/target \
     cargo build --release --bin rezolus)
   ```

3. **Run smoke against each parquet of interest on both branches,**
   using distinct `--port` and `--out` per run so they don't collide.
   Bump `--wait-ms` (default 4000) if charts are still painting.

   ```bash
   bash scripts/viewer_chromium_smoke.sh \
     --port 18510 --out /tmp/smoke_cur --wait-ms 6000 \
     site/viewer/data/cachecannon.parquet

   REZOLUS_BIN=/tmp/preplan/target/release/rezolus \
     bash /tmp/preplan/scripts/viewer_chromium_smoke.sh \
     --port 18520 --out /tmp/smoke_pre --wait-ms 6000 \
     /tmp/preplan/site/viewer/data/cachecannon.parquet
   ```

4. **Pixel-diff every screenshot route-by-route** with PIL:

   ```bash
   pip install --user Pillow
   python3 <<'PY'
   from PIL import Image, ImageChops
   from pathlib import Path
   pre = Path('/tmp/smoke_pre/shots')
   cur = Path('/tmp/smoke_cur/shots')
   for f in sorted(p.name for p in pre.iterdir() if p.suffix == '.png'):
       a, b = pre / f, cur / f
       if not b.exists(): print(f'{f}: MISSING in cur'); continue
       ia = Image.open(a).convert('RGB'); ib = Image.open(b).convert('RGB')
       if ia.size != ib.size: print(f'{f}: size mismatch'); continue
       diff = ImageChops.difference(ia, ib)
       if diff.getbbox() is None:
           print(f'{f}: identical')
       else:
           px = list(diff.getdata())
           nz = sum(1 for r,g,b in px if r or g or b)
           pct = 100.0 * nz / len(px)
           print(f'{f}: {nz} px differ ({pct:.2f}%)')
   PY
   ```

5. **For non-identical routes, save a side-by-side composite** so you
   can eyeball what changed:

   ```python
   from PIL import Image
   a = Image.open('/tmp/smoke_pre/shots/overview.png').convert('RGB')
   b = Image.open('/tmp/smoke_cur/shots/overview.png').convert('RGB')
   out = Image.new('RGB', (a.width * 2, a.height), 'white')
   out.paste(a, (0, 0)); out.paste(b, (a.width, 0))
   out.save('/tmp/diff_overview.png')
   ```

### Interpreting results

- **Byte-identical PNGs**: chromium rendering is deterministic across
  runs (verify by running smoke twice on the same branch — should
  produce zero pixel diffs). If diffs appear, they are behavior
  changes, not noise.
- **Single-node parquets** (e.g. `site/viewer/data/demo.parquet`):
  expect byte-identical PNGs across every route for a refactor that
  doesn't change semantics.
- **Multi-node parquets** (e.g. `site/viewer/data/cachecannon.parquet`):
  the top-nav node picker auto-defaults to the first node and threads
  it through every query (R4 N3). Expect visible diffs from any change
  that touches per-node view materialization (`_src_node_<X>`) or the
  client-side node-threading path.
- **The smoke harness's silent-section detector** flags `/cgroups` as
  empty (`wrappers=0, placeholders=0, notes=0`) when no cgroup is
  selected — pre-existing on `main`. Confirm by re-running the smoke
  step on the baseline worktree; it should report the same failure.

### When to run this check

- Edits to `src/viewer/assets/lib/charts/`, `viewer_core.js`, or
  `data.js`'s data-shape projections.
- Edits to `crates/dashboard/src/sql.rs` or `crates/dashboard/src/dashboard/*.rs`
  that change emitted SQL.
- Edits to `metriken-query-sql`'s view generation
  (`render_src_sql`, `render_per_source_views_sql`,
  `render_per_node_views_sql`).
- Before opening a PR that touches any of the above.

### Skip routes

Default skips: `/query` (interactive Query Explorer — no charts to
render), `/metadata`, `/notebook`, `/selection`, `/report`
(text-only). Pass `--skip` to add more.
