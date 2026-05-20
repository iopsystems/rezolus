//! Path-shaped adapter over the shared `report-save` crate. The
//! server's `save_with_selection` handler receives a `PathBuf` (the
//! loaded parquet on disk); this module reads the path into bytes
//! once and delegates the trim/embed/tarball logic to the shared
//! crate so the WASM static-site viewer can share it.

use std::path::Path;

use bytes::Bytes;

use crate::parquet_metadata::AbContainers;

pub use report_save::ReportPayload;

fn read_path_to_bytes(path: &Path) -> Result<Bytes, Box<dyn std::error::Error>> {
    Ok(Bytes::from(std::fs::read(path)?))
}

/// Trim-free single-parquet save. Reads the source parquet, embeds
/// the selection JSON in the footer, returns the new bytes.
pub fn save_single_parquet_embed_only(
    source_path: &Path,
    selection_json: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = read_path_to_bytes(source_path)?;
    report_save::save_single_parquet_embed_only(bytes, selection_json).map_err(Into::into)
}

/// SQL-backed single-parquet save with column trim. Path-shaped
/// adapter over `report_save::save_single_parquet_sql`. Works with
/// any `SqlCapture`.
pub fn save_single_parquet_sql(
    source_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    catalog: &metriken_query_sql::MetricCatalog,
    trim_columns: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = read_path_to_bytes(source_path)?;
    report_save::save_single_parquet_sql(bytes, payload, selection_json, catalog, trim_columns)
        .map_err(Into::into)
}

/// Trim-free combined-A/B save. Both per-side parquets are embedded
/// with the selection JSON and repacked into the tarball.
pub fn save_combined_ab_tarball_embed_only(
    baseline_path: &Path,
    experiment_path: &Path,
    selection_json: &str,
    manifest: &AbContainers,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let baseline_bytes = read_path_to_bytes(baseline_path)?;
    let experiment_bytes = read_path_to_bytes(experiment_path)?;
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    report_save::save_combined_ab_tarball_embed_only(
        baseline_bytes,
        experiment_bytes,
        selection_json,
        &manifest_bytes,
    )
    .map_err(Into::into)
}

/// SQL-backed combined-A/B save with per-side column trim. Path-
/// shaped adapter over `report_save::save_combined_ab_tarball_sql`.
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball_sql(
    baseline_path: &Path,
    experiment_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_catalog: &metriken_query_sql::MetricCatalog,
    experiment_catalog: &metriken_query_sql::MetricCatalog,
    manifest: &AbContainers,
    trim_columns: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let baseline_bytes = read_path_to_bytes(baseline_path)?;
    let experiment_bytes = read_path_to_bytes(experiment_path)?;
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    report_save::save_combined_ab_tarball_sql(
        baseline_bytes,
        experiment_bytes,
        payload,
        selection_json,
        baseline_catalog,
        experiment_catalog,
        &manifest_bytes,
        trim_columns,
    )
    .map_err(Into::into)
}
