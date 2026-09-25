//! The `.rez` archive format: container, writers, and reader.
//!
//! Extracted from the `rezolus` binary so the WASM viewer can read the same
//! archives the binary writes. `rezolus` is a binary-only crate with no `lib`
//! target, so nothing could depend on it; a `.rez` reader living there was
//! reachable by the server viewer and by nothing else, which is why the
//! static-site viewer has never opened a `.rez` at all.
//!
//! Nothing here knows about samplers, endpoints, or the CLI — it speaks in
//! recordings, tables, segments, and bytes.

/// Where `rez` ends and `dendro` begins, asserted rather than assumed.
/// Tests only; no production code depends on dendro yet. See #1224.
mod dendro_compat;
/// The half of that boundary dendro 0.2.0 added: a rezolus-shaped tick through
/// `Frame`, the wire codec, and a `Subscriber`. Tests only, same as above — it
/// is what tells us a dendro release still fits before #1224 Phase 2 leans on
/// it.
mod dendro_replicate_contract;
/// What a group's slots mean, and when that changed — the blob dendro's
/// `caller_rows` holds. Not behind `write`: a reader has to decode identity to
/// make sense of an archive's values, so this is read-path code.
pub mod index;
/// A group table split by occupant through that index, for the reader. Not
/// behind `write` for the same reason.
pub mod indexed;
/// Reshape a plain parquet into `.rez` recordings (metriken-free). Not behind
/// `write` — the browser assembles `.rez` reports from uploaded parquet bytes.
pub mod parquet_ingest;
pub mod reader;
pub mod rez;
pub mod rez_sqlite;
/// The tar (v1/v2) `.rez` writer, kept only so tests can build v1/v2 fixtures.
/// Nothing ships that writes this container any more.
#[cfg(any(test, feature = "test-support"))]
pub mod rez_stream;
/// v3 rewrite tooling (`combine`/`filter`/`upgrade`) and report assembly.
///
/// Not behind `write`: everything here is metriken-free (catalog `UPDATE`s,
/// verbatim BLOB copies, arrow-level segment projection, WAL-tail
/// materialization), so the browser viewer can assemble/trim a `.rez` report
/// in the reader build. The metriken-dependent ingest (agent snapshot → rows)
/// lives in `rez_v3_writer`, which stays `write`-gated.
pub mod rez_v3_rewrite;
/// The v3 streaming writer.
#[cfg(feature = "write")]
pub mod rez_v3_writer;
pub mod schema;
pub mod seal_policy;
/// A v3 `.rez` rewritten as a dendro archive. Behind `write` because it
/// needs dendro's append side.
#[cfg(feature = "write")]
pub mod to_dendro;
pub mod wal;
pub mod window;
pub mod wire;
