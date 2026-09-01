# Retire the `.parquet.ab.tar` container in favour of `.rez`

- **Opened:** 2026-08-27
- **Status:** OPEN — intent-first, pre-build. One GO/NO-GO gate stands between
  this and an ordinary migration: whether the browser-only viewer can read a v3
  (SQLite) `.rez` at an acceptable bundle and load cost. **Gate wave 1 measured
  2026-08-27: bundle cost is not disqualifying (+425 KB gzipped, 1.277×), and
  the build "just works" — but the measurement moved the question from bundle
  size to the VFS memory model.** See "Gate — wave 1" below. Nothing else is
  built yet.

  **PARKED 2026-09-01**, at the point wave 1 left it: the design, the four
  `--ab` call sites, the workstream split, and the wave-1 measurement are all
  recorded here, and wave 2 is specified but not started. *Restart condition:*
  someone picks up gate wave 2 (peak wasm memory on real archives / the
  engine-free read path). Nothing decays in the meantime except the argument
  for acting early — the tarball corpus keeps growing while both writers are
  live, which is the one cost of parking.
- **Arc:** consumes the `.rez` container work
  ([per-sampler archive](2026-07-13-per-sampler-rez-archive.md),
  [SQLite container](2026-08-12-rez-sqlite-container.md),
  [acquisition groups / Stage 4](2026-08-20-stage4-native-v3-ingest.md)) and
  closes out the A/B half of
  [A/B compare mode](2026-04-21-ab-compare-mode.md). Picks up the **N-way
  faceting** deferral recorded in
  [`.rez` reader ecosystem](2026-07-15-rez-reader-ecosystem.md), which this
  effort puts on the critical path rather than leaving as a someday item.
- **Owner:** unassigned — parked pending pickup (expected: Brian Martin).
  Opened and measured through wave 1 by Yao Yue.
- **Repos:** rezolus only (`src/parquet_tools/`, `src/viewer/`,
  `crates/viewer/`, `crates/report-save/`).

## Why

Two containers now describe the same thing — two captures compared side by
side. `.parquet.ab.tar` predates `.rez` and is a tar of
`baseline.parquet` + `experiment.parquet` + an `ab.json`
(`src/viewer/ab_extract.rs:1-35`, `src/parquet_metadata.rs:127`). `.rez` does
the job better for the rezolus-native case: `combine baseline.rez
experiment.rez -o ab.rez` (`src/parquet_tools/combine.rs:312`,
`combine_rez_v3`) assembles a multi-recording archive that keeps per-sampler
cadence and real acquisition windows on both arms, and identifies each side by
a **label set** rather than by a `baseline=<source-name>` mapping the user must
look up with `recording metadata --field source`. The viewer already renders a
two-recording archive as an A/B comparison and sniffs `.rez` by content ahead
of the tarball sniffer on both the file path (`src/viewer/mod.rs:483`) and the
upload path (`src/viewer/actions.rs:249`).

Keeping both is not free. Every A/B surface branches twice, `AbContainers` is a
second manifest schema to version, and a user choosing a comparison format has
to know which of two answers applies to their inputs.

The timing argument is the decisive one, and it came from the discussion that
opened this entry: **the artifact is user-held.** People save a
`.parquet.ab.tar` and share it. The cost of retirement is therefore
proportional to how many exist, which grows for as long as we keep writing
them. Migrating before wider adoption is materially cheaper than migrating
after, and that is a window that closes on its own.

## Goal

One comparison container. `.rez` is it. At the end, `--ab`,
`save_combined_ab_tarball`, `AbContainers`, and the `.ab.tar` branches in
`viewer_api.js` / `selection.js` / `ab_filename.js` are deleted, and
`ab_extract` survives for a release as a read-only path before going too.

Explicitly **not** a goal: preserving read compatibility for `.parquet.ab.tar`
indefinitely. That was weighed and declined — see Decisions.

## Decision criteria (the gate)

**GATE — can the WASM viewer read a v3 `.rez`?** `crates/viewer` links no
rusqlite and no rez reader today; it handles parquet and `.parquet.ab.tar` and
nothing else. The main binary gets SQLite from `rusqlite` with `bundled`, which
compiles SQLite from C source (`Cargo.toml:91-93`), on a wasm bundle whose
build already runs `opt-level = "s"` and strips debuginfo for size
(`Cargo.toml:172-175`).

Measure before building anything else, in the shape of the
[split-table gate](2026-08-18-split-table-read-cost-gate.md): a throwaway spike
that opens a real v3 archive in the browser and reports

- delta in shipped bundle bytes (gzipped, as served),
- cold time to first chart on a recording of a realistic size,
- peak browser memory while doing it.

Thresholds to be set before the spike runs, not after it reports.

Two candidate implementations, and the spike should say which is being
measured. (a) Compile `rusqlite`/SQLite to `wasm32-unknown-unknown`. (b) Read
the container directly without a SQL engine: the v3 design records that SQLite
is used here "as a transactional allocator with a queryable catalog, not as a
query engine" ([SQLite container entry](2026-08-12-rez-sqlite-container.md)),
and a read-only consumer only needs the catalog and the segment BLOBs.

**NO-GO consequence.** If neither route is acceptable, `.parquet.ab.tar` — or
some parquet-only share format — survives as the browser's save format, and
this effort reduces to documentation plus the server-side half. That is a real
possible outcome and the reason the gate runs first: workstreams A and B below
are otherwise built on a premise that has not been tested.

## Gate — wave 1 (bundle cost), measured 2026-08-27

Cheap arm first: does it build at all, and what does it weigh? Run on Linux
x86_64, stock Ubuntu clang 18.1.3, `wasm32-unknown-unknown`, the same profile
overrides `crates/viewer/build.sh` exports (`opt-level = "s"`, no debuginfo).

**It builds, with no special toolchain.** `rusqlite 0.40` does *not* use
`libsqlite3-sys` on `wasm32-unknown-unknown` — it resolves instead to
`sqlite-wasm-rs 0.5.5` + `rsqlite-vfs 0.1.1`, which are purpose-built wasm
bindings to libsqlite3. No emscripten, no Homebrew LLVM, no patched target. A
standalone probe crate (open in-memory db, create a `(id, BLOB)` table, read it
back) compiled in **17.7 s** on the first try.

| Arm | raw `.wasm` | gzip -9 |
| --- | --- | --- |
| empty `cdylib` (floor) | 364 B | — |
| SQLite probe alone | 1,472,577 B | 532,271 B |
| `crates/viewer` today | 6,648,965 B | 1,537,087 B |
| `crates/viewer` + rusqlite | 7,677,898 B | 1,962,402 B |
| **delta** | **+1,028,933 B (1.155×)** | **+425,315 B (1.277×)** |

Method caveats, both of which make these **upper bounds** rather than shipping
figures: `wasm-opt` is not installed here, and neither arm went through
`wasm-bindgen`'s post-processing, so absolute sizes are inflated. Both arms were
built identically, so the delta is the comparable number. The shim added to
`crates/viewer` exercised only open / create / insert / select — enough to keep
LTO from stripping the payload, not enough to represent a full reader. The
spike's edits to `crates/viewer/Cargo.toml`, `crates/viewer/src/lib.rs` and
`Cargo.lock` were reverted; nothing from it is in the tree.

**The finding that matters is not the size.** `sqlite-wasm-rs` ships three VFS
implementations, and the choice is consequential:

| VFS | Storage | Context | Durability |
| --- | --- | --- | --- |
| `memory` (default) | RAM | any | full |
| `sahpool` | OPFS | **dedicated worker only** | full |
| `relaxed-idb` | IndexedDB | any | relaxed |

The default path therefore holds the entire archive in wasm linear memory —
which is precisely the property the
[SQLite container entry](2026-08-12-rez-sqlite-container.md) rejected tar for
("the reader slurps the **entire archive into memory** … fine at 7 MB and
untenable at the 605 MB the fleet run produced"). Adopting SQLite in the browser
naively would reintroduce it, on a platform with a 4 GB address-space ceiling
and far less headroom in practice. `sahpool` avoids it but requires the viewer
to run its reader in a dedicated worker *and* to copy a user-picked file into
OPFS before opening it — an architecture change, and a full write of the
archive up front.

### What a VFS is, and why it decides this

Written down because the next person to pick this up should not have to
rediscover why the table above is the whole gate.

A VFS is SQLite's shim between the SQL engine and the operating system. SQLite
never calls `open`/`read`/`write` directly; it goes through a `sqlite3_vfs`
struct of function pointers — `xOpen`, `xRead`, `xWrite`, `xSync`, `xFileSize`,
`xLock`/`xUnlock`, `xRandomness`, `xCurrentTime` — which is how one codebase
serves unix and win32. Built with `SQLITE_OS_OTHER=1` there is no default VFS
at all and the embedder supplies one, which is the situation on
`wasm32-unknown-unknown`: there is no OS underneath. `rsqlite-vfs` exports
`SQLiteVfs` / `SQLiteIoMethods` / `VfsStore` / `VfsFile` / `register_vfs`
precisely as an invitation to write one.

Two properties of that interface decide the outcome:

1. **SQLite reads in pages** (4 KB by default) and asks the VFS for byte
   ranges. So the VFS — not the engine — determines whether opening a 600 MB
   archive touches 600 MB or touches the handful of catalog rows and segment
   BLOBs a chart actually needs. A `.rez` is unusually well suited to the
   latter: the segments are large and immutable, and a query wants few of them.
2. **`xRead` is synchronous.** It must return the bytes before it returns.
   Browser file APIs are asynchronous — `Blob.slice().arrayBuffer()` is a
   promise — so a synchronous SQLite read cannot be serviced from an async web
   API on the main thread. Every row of the VFS table is a different way of
   paying that debt: the memory VFS pre-loads everything so no read is ever
   async; OPFS `sahpool` uses `FileSystemSyncAccessHandle`, which is genuinely
   synchronous but exists **only in a dedicated worker** (that is where the
   worker requirement comes from — it is not arbitrary); `relaxed-idb` buys
   sync semantics from an async store by relaxing durability.

**Wave 1 verdict:** bundle cost does not disqualify the plan. The gate is *not*
passed. The open question is now the memory model, and wave 2 should measure
it directly rather than reason about it:

- peak wasm memory opening a real v3 `.rez` under the memory VFS, at several
  archive sizes, to find where it stops being viable;
- whether a read-only consumer can skip the SQL engine entirely (candidate (b)
  in the criteria above) and stream segment BLOBs out of the container — which
  would sidestep both the VFS question and most of the 425 KB;
- **a `FileReaderSync` VFS** — untested hypothesis, recorded because it is the
  cheapest thing that would resolve point 2 above without a copy. Workers also
  expose `FileReaderSync`, a *synchronous* read over a `Blob`. A read-only VFS
  built on it over the user's picked `File` would give random access with no
  copy into OPFS and no whole-archive RAM load — page in exactly what is read.
  Unverified on two counts: whether it performs acceptably, and whether
  `sqlite-wasm-rs`'s trait surface accommodates it. Still needs a worker.
- if none of those, the cost of the dedicated-worker + OPFS restructure.

## Scope — what `--ab` is actually carrying

Four uses, established by reading the call sites:

1. **CLI producer.** `recording combine --ab` (`combine.rs:625`,
   `write_ab_tarball` at `:681`).
2. **Server viewer save.** Compare-mode Save-as-Report
   (`src/viewer/actions.rs:839` → `crates/report-save/src/lib.rs:149`).
3. **Browser viewer save.** The same, packed client-side
   (`crates/viewer/src/lib.rs:851`).
4. **Load path.** `looks_like_ab_tarball` / `extract_ab_tarball`
   (`src/viewer/ab_extract.rs:35,62`), reached from both file mode
   (`src/viewer/mod.rs:492`) and upload (`src/viewer/actions.rs:255`).

(1) is already superseded for rezolus-vs-rezolus inputs. (2) and (3) are the
writers that grow the corpus. (4) is cleanup.

## Workstreams

### A. `.rez` must hold what a saved report holds

Two gaps, both additive to the catalog, neither a format break:

- **Selection and events.** `KEY_SELECTION` and `KEY_EVENTS` are parquet footer
  keys (`src/parquet_metadata.rs`). In a `.rez` they belong in the manifest.
  `annotate` already established the shape — KPIs are a catalog column, so
  embedding them is an `UPDATE` per recording rather than a rewrite (#1073).
- **Column-level trim.** `filter` on a `.rez` drops whole tables by *sampler*;
  Save-as-Report needs per-column projection
  (`trim_parquet_to_columns`, `crates/report-save/src/lib.rs`). For `.rez` that
  means decode → project → re-encode segments, which breaks the "catalog
  operation, segment BLOBs move verbatim" property `rez_v3_rewrite` has held
  since #1073. This is the only genuinely new machinery in the plan, and it
  should be written knowing it is the exception to that rule.

### B. Sides that are not rezolus

An `.ab.tar` side may be a row-merged **multi-source** parquet — service plus
loadgen. `AbSide.sources: Vec<String>` exists for exactly that, and
`AbContainers.category` carries `inference-library`, so the
vllm-vs-sglang comparison is a live user of the tarball. Two sub-parts, and
only one is hard:

- **Ingest — less blocked than it looks.** The recorder already converts a
  Prometheus scrape into Snapshots (that is how `-o out.raw` works against a
  Prometheus source), and `demote_from_rez`'s own documentation frames the
  `.rez` refusal as a policy call about what a `.rez` *means*
  (`src/recorder/mod.rs:915-931`), not an ingest limitation. The honest cost is
  semantic: such tables have no true acquisition window, so the reader must
  carry a windowless table and report **no** band rather than a fabricated one.
- **Shape — the hard part.** The natural `.rez` encoding of a two-arm,
  two-source comparison is four recordings labelled `{arm, source}`.
  **Producing** one got easier while this entry sat parked:
  [multi-endpoint `.rez`](2026-08-28-multi-endpoint-rez-record.md) shipped
  2026-08-28, so `record --endpoint a --endpoint b -o out.rez` now writes a
  recording per endpoint instead of demoting to parquet, and the demotion is
  keyed on a Prometheus *source* rather than on endpoint count. **Consuming**
  one did not change: the viewer still maps a two-recording archive onto
  baseline/experiment and shows the first two beyond that
  (`src/viewer/mod.rs:97-99`). Simultaneous N-way faceting was
  deferred in [the reader-ecosystem entry](2026-07-15-rez-reader-ecosystem.md)
  as a capture-model refactor plus frontend work, and is already tracked as
  **N-way compare (N > 2)** in [the backlog](../backlog.md). This effort
  promotes it from someday to prerequisite.

### C. The browser reader

The gate above. Nothing else in this plan should start before it reports.

## Decisions

- **Read compatibility for existing `.parquet.ab.tar` is not preserved
  indefinitely.** Weighed and declined on the grounds that adoption is early
  enough that the corpus is small, and it will only ever be larger. Recorded
  because it is the assumption the whole plan rests on: if the format turns out
  to be more widely held than believed, this decision — not the technical work
  — is what has to be revisited.
- **Separate write retirement from read retirement, and do the write side
  first.** The number of artifacts in the wild is bounded by when we stop
  *emitting* them, not by when we stop reading them. Reading can linger a
  release and then go.
- **Sequence the gate first**, following #1065: the unknown that can invalidate
  the plan gets measured before work is built on top of it.

## Ordering

1. **Gate (C).** WASM `.rez` read spike; thresholds fixed in advance.
2. **Docs, immediately and independently of the gate.** `combine --ab`'s
   `long_about` should point rezolus-vs-rezolus users at
   `combine a.rez b.rez -o ab.rez`, and `.rez` should be added to the file
   picker's `accept` list (`src/viewer/assets/lib/ui/layout.js:26`) — it is
   absent from both viewers even though the server ingests it fine by content
   sniffing. Cheap, and it slows corpus growth while the gate runs.
3. **A** — manifest selection/events + column trim. Unblocks the server-side
   writer (2).
4. **B ingest** — windowless tables end to end, with the reader declining to
   band them.
5. **B shape** — the capture-model refactor (N-way).
6. **Delete** the writers, then `ab_extract` a release later.

## Deferred or reopen items

- Thresholds for the gate are not yet set. They must be written down before the
  spike runs.
- Whether a windowless `.rez` table should be *recordable* by `record` at all,
  or only *constructible* by `combine`, is open. The narrower option keeps
  "a `.rez` is what a rezolus agent produced" true for anything the recorder
  writes.
- If the gate is NO-GO, open a follow-on entry for a browser-side share format
  rather than reopening this one in place.

## Appendix: Skills Invoked

- `engineering-journal` — opened this entry, and reconciled the index and
  backlog cross-references.

The roster covers this entry's authoring only; the effort has not started.
