//! Path-shaped adapter over the shared `report-save` crate. The
//! server's `save_with_selection` handler receives a `PathBuf` (the
//! loaded parquet on disk) and a MetricsSource; this module reads
//! the path into bytes once and delegates the trim/embed/tarball logic
//! to the shared crate so the WASM static-site viewer can share it.

use std::path::Path;

use bytes::Bytes;
use metriken_query::MetricsSource;

use crate::parquet_metadata::AbContainers;

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

/// Combined-A/B equivalent.
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball(
    baseline_path: &Path,
    experiment_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    manifest: &AbContainers,
    trim_columns: bool,
    baseline_source: &dyn MetricsSource,
    experiment_source: &dyn MetricsSource,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let baseline_bytes = read_path_to_bytes(baseline_path)?;
    let experiment_bytes = read_path_to_bytes(experiment_path)?;
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    report_save::save_combined_ab_tarball(
        baseline_bytes,
        experiment_bytes,
        payload,
        selection_json,
        baseline_source,
        experiment_source,
        &manifest_bytes,
        trim_columns,
    )
    .map_err(Into::into)
}
