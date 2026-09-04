//! Path-shaped adapter over the shared `report-save` crate. The
//! server's `save_with_selection` handler receives a `PathBuf` (the
//! loaded parquet on disk) and a MetricsSource; this module reads
//! the path into bytes once and delegates the trim/embed logic to the
//! shared crate so the WASM static-site viewer can share it. Compare and
//! `.rez`-source saves call the shared `.rez` builders directly.

use std::path::Path;

use bytes::Bytes;
use metriken_query::MetricsSource;

pub use report_save::ReportPayload;

fn read_path_to_bytes(path: &Path) -> Result<Bytes, Box<dyn std::error::Error>> {
    Ok(Bytes::from(std::fs::read(path)?))
}

/// HTTP-friendly wrapper: read the source parquet from disk, then
/// delegate to the shared crate.
pub fn save_single_parquet(
    source_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    source: &dyn MetricsSource,
    trim_columns: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = read_path_to_bytes(source_path)?;
    report_save::save_single_parquet(bytes, payload, selection_json, source, trim_columns)
        .map_err(Into::into)
}
