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

pub mod reader;
pub mod rez;
pub mod rez_sqlite;
/// The tar (v1/v2) `.rez` writer, kept only so tests can build v1/v2 fixtures.
/// Nothing ships that writes this container any more.
#[cfg(any(test, feature = "test-support"))]
pub mod rez_stream;
pub mod rez_v3_rewrite;
pub mod rez_v3_writer;
pub mod seal_policy;
