//! Path-shaped adapter over the shared `report-save` crate. The
//! server's `save_with_selection` handler receives a `PathBuf` (the
//! loaded parquet on disk) and an in-memory `Tsdb`; this module reads
//! the path into bytes once and delegates the trim/embed/tarball logic
//! to the shared crate so the WASM static-site viewer can share it.

use std::path::Path;
use std::sync::Arc;

use metriken_query::{Bytes, Tsdb};
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

/// Combined-A/B equivalent: reads both per-side parquets from disk,
/// serializes the manifest, hands off to the shared crate which emits
/// a `baseline.parquet + experiment.parquet + ab.json` tar.
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
