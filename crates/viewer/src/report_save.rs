//! WASM-side adapter over the shared `report-save` crate.
//!
//! The single-parquet save still goes through here (a thin pass-through);
//! compare and `.rez`-source saves call the shared crate's `.rez` report
//! builders directly from `lib.rs`. The tarball repack (and its synthetic
//! `AbContainers` manifest) is gone — a compare now saves a `.rez`.

use bytes::Bytes;
use metriken_query::MetricsSource;

pub use report_save::ReportPayload;

/// Trim or embed-only single-parquet save.
pub fn save_single_parquet(
    source_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    source: &dyn MetricsSource,
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    report_save::save_single_parquet(source_bytes, payload, selection_json, source, trim_columns)
}
