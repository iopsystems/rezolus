//! Path-shaped adapter over the shared `report-save` crate. The
//! server's `save_with_selection` handler receives a `PathBuf` (the
//! loaded parquet on disk); this module reads the path into bytes
//! once and delegates the trim/embed/tarball logic to the shared
//! crate so the WASM static-site viewer can share it.
//!
//! The trim path requires a `Tsdb` (PromQL `engine.columns(...)` is
//! how kept columns are resolved) and is therefore gated behind the
//! `live-mode` feature. The embed-only path is unconditional.

use std::path::Path;
#[cfg(feature = "live-mode")]
use std::sync::Arc;

use bytes::Bytes;
#[cfg(feature = "live-mode")]
use metriken_query::Tsdb;
#[cfg(feature = "live-mode")]
use parking_lot::RwLock;

use crate::parquet_metadata::AbContainers;

pub use report_save::ReportPayload;

fn read_path_to_bytes(path: &Path) -> Result<Bytes, Box<dyn std::error::Error>> {
    Ok(Bytes::from(std::fs::read(path)?))
}

/// HTTP-friendly wrapper: read the source parquet from disk, then
/// delegate to the shared crate. The original body is embedded
/// verbatim under `selection` in the output footer so re-opening the
/// report restores the saved Notebook state.
#[cfg(feature = "live-mode")]
pub fn save_single_parquet(
    source_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    tsdb: &Arc<RwLock<Tsdb>>,
    trim_columns: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = read_path_to_bytes(source_path)?;
    let tsdb_read = tsdb.read();
    report_save::save_single_parquet(bytes, payload, selection_json, &tsdb_read, trim_columns)
        .map_err(Into::into)
}

/// Trim-free single-parquet save. Reads the source parquet, embeds
/// the selection JSON in the footer, returns the new bytes. SQL-only
/// callers use this directly; the live-mode dispatch also falls
/// through to it when the baseline slot is SQL-backed.
pub fn save_single_parquet_embed_only(
    source_path: &Path,
    selection_json: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = read_path_to_bytes(source_path)?;
    report_save::save_single_parquet_embed_only(bytes, selection_json).map_err(Into::into)
}

/// Combined-A/B equivalent: reads both per-side parquets from disk,
/// serializes the manifest, hands off to the shared crate which emits
/// a `baseline.parquet + experiment.parquet + ab.json` tar.
#[cfg(feature = "live-mode")]
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball(
    baseline_path: &Path,
    experiment_path: &Path,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_tsdb: &Arc<RwLock<Tsdb>>,
    experiment_tsdb: &Arc<RwLock<Tsdb>>,
    manifest: &AbContainers,
    trim_columns: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let baseline_bytes = read_path_to_bytes(baseline_path)?;
    let experiment_bytes = read_path_to_bytes(experiment_path)?;
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    let baseline_read = baseline_tsdb.read();
    let experiment_read = experiment_tsdb.read();
    report_save::save_combined_ab_tarball(
        baseline_bytes,
        experiment_bytes,
        payload,
        selection_json,
        &baseline_read,
        &experiment_read,
        &manifest_bytes,
        trim_columns,
    )
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
