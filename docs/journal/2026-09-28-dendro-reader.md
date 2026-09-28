# Reading dendro archives

- **Opened:** 2026-09-28
- **Status:** **OPEN — 2a built, 2b next.** Step 2 of the 6.0 plan (#1224):
  a reader for dendro archives, before the writer produces them. 2a (the
  `.rez` layout in a dendro container) reads, and so does 2b (the long
  layout and its occupant stream). Materializing a long table's WAL tail
  belongs to the writer (step 3).
- **Owner:** Brian Martin

## Goal

`rezolus view`, `mcp` and the static-site viewer open a dendro archive as
they open a `.rez`, by content, and answer every query the same way. The
reader uses dendro's API; anything it needs that dendro lacks is added to
dendro (decided 2026-09-25).

Two archive shapes have to read:

1. **What `recording upgrade --to dendro` writes today** (#1301): the `.rez`
   layout copied into a dendro container. Tables become streams under the
   same keys, segments keep their bytes, WAL rows keep their `.rez` msgpack
   encoding, and the identity index keeps its `IndexEntry` blobs in
   `caller_rows`. This is the `.rez` v3 layout in a different container.
2. **The 6.0 layout** ([the layout entry](2026-09-25-dendro-archive-layout.md)):
   every group with slots long, keyed by occupant, with its occupant table
   in a parquet stream beside it. metriken-query reads long segments from
   0.31.0 (iopsystems/metriken#165); occupant labels reach it through
   `ColumnRelabel`, as slot labels do today.

## Design

**One seam.** `RezReader` touches its container through about ten methods of
`RezDb` (`crates/rez/src/rez_sqlite.rs`): recordings, samplers, segment
catalog and bytes, the live WAL and its span, and caller rows. dendro's
`Archive` has an equivalent for each (`read_sources`, `all_streams`,
`read_segment_meta`, `read_segment_bytes`, `segment_span`, `live_wal`,
`live_wal_span`, `read_caller_rows`, `caller_row_streams`), except "the last
caller row at or before a time that a predicate accepts", which the indexed
reader uses to find the `Full` entry to replay from. That is added to dendro
(iopsystems/dendro#23).

The seam becomes a trait in `crates/rez`, implemented for `RezDb` and for
dendro's `Archive`. The reader's `SegmentSource`/`DbHandle` hold either. A
dendro source is a recording, a stream is a table; dendro's `i64` timestamps
are converted at the trait, and a negative one is an error.

**Detection by content.** `open_recordings` and `open_recordings_from_bytes`
recognize a dendro archive with `dendro::archive::sniff`/`sniff_bytes` before
the `.rez` checks, so the CLI and the browser take the same path. `RezDb`
keeps refusing a dendro archive by name, for every caller that is not the
reader.

**Step 2a, in this change:** the trait, the dendro implementation, and
detection, reading shape 1. The oracle is exact: a `.rez` converted with
#1301 must answer every query the same as the original.

**Step 2b, next:** shape 2. The occupant table's encoding is settled (the
layout entry, "The occupant stream": a parquet stream beside its data
stream); it needs a writer or fixture that produces long segments and their
occupant stream.

## 2a: what was built

- `crates/rez/src/catalog.rs`: the `Catalog` trait, `impl Catalog for
  RezDb`, and `DendroCatalog`, which converts dendro's `i64` timestamps and
  refuses a negative one. `Container` decides by content (dendro's `sniff`)
  and opens either.
- `RezReader`'s segment sources and connection handle hold a `dyn
  Catalog`, and a path remembers its `Container` so a lazy reopen reads it
  as the same one. `open_recordings`, `open_with_pool` and
  `open_recordings_from_bytes` check for dendro first.
- dendro 0.3.1 adds `last_caller_row_at_or_before` (iopsystems/dendro#23).

**Checked.** Unit tests convert fixtures with #1301 and require the dendro
archive to answer as the `.rez` did: names, labels, span, sample timestamps,
interval and every query, for counter, gauge and histogram group tables and a
query across them, from a path and from bytes; and for a table read through
the identity index, with column labels, without them, and with the index cut
back to its restatement. On two real recordings (581 MB, 2.3 h; 1.28 GB,
9.6 h), converted in 2.6 s and 7.0 s, six `mcp query` expressions
(cpu, syscall, syscall latency, per-cgroup and per-thread) gave the same
output from both, once `sum by` output was sorted: its series order is not
defined.

## 2b: what was built

- The occupant stream's format, for the reader and the writer:
  `encode_segment`/`decode_segment` (label columns are `UInt64` when every
  value converts exactly, decimal or the agent's 16-digit `__uid__` hex,
  marked in field metadata so the text comes back byte for byte; `Utf8`
  otherwise) and the WAL row (`encode_wal_row`, msgpack of the tick's
  occupants). Written here first, it lives in metriken-segment 0.1.0
  (`metriken_segment::occupants`), per metriken's
  `docs/journal/2026-09-28-high-cardinality-stack.md`.
- `OccupantLabels`, the `ColumnRelabel` that adds an occupant's labels to
  its series and keeps `__occupant__` on them, is in metriken-query 0.32.0
  (`metriken_query::long`). `crates/rez/src/occupants.rs` re-exports both.
- `RezReader`: a stream named `<table>/occupants` is not a table; the table
  it names reads through `OccupantLabels`, built from the stream's sealed
  segments and live WAL. A filter on an occupant label (`comm`, `pid`)
  comes off the segment filter and applies to the relabelled series.

**Checked.** A long table with its occupant stream and the wide table with
labels in its columns are written from the same observations (two segments,
a TID reused by a second occupant with its own `__uid__`, a restatement),
and every query agrees once `__occupant__` is set aside: rates per series,
`sum`, `sum by (comm)`, `sum by (tgid)`, filters on `comm` and `pid`; the
reused TID reads as two series. Also from bytes, and with an occupant's
labels only in the occupant stream's WAL. With the relabel disabled, all
three tests fail.

**Not yet:** the WAL tail of a long table. `materialize_wal_tail` builds a
wide table from `WalGroupRow`s; a long table's tail waits for the writer,
which decides what its WAL rows are.

## What stays out

- `recording metadata`, `filter`, `combine` and the other `recording`
  subcommands keep reading `.rez` only. They move with the 6.0 writer.
- The static-site viewer builds `crates/rez` without `write`, and dendro
  without `write` is reader-only and builds for wasm32; the dendro path
  needs no new wasm constraint.
