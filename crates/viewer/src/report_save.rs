//! WASM-side adapter over the shared `report-save` crate. The shared
//! crate handles parquet projection + tar repack against `Bytes`; this
//! thin wrapper synthesizes an `AbContainers` manifest from the two
//! attached viewers (compare mode loads two separate parquets, never
//! a real tar manifest) and serializes it for the shared crate.

use bytes::Bytes;
use metriken_query::MetricsSource;
use serde::Serialize;

pub use report_save::ReportPayload;

/// Wire shape of the AB tarball manifest. Defined here rather than
/// pulled from `crate::parquet_metadata` because the WASM crate
/// doesn't depend on the rezolus binary. The shared `report-save`
/// crate is intentionally manifest-agnostic — it takes pre-serialized
/// bytes — so this struct never crosses a boundary.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AbContainers {
    pub version: u32,
    pub baseline: AbSide,
    pub experiment: AbSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct AbSide {
    pub alias: String,
    pub sources: Vec<String>,
}

impl AbContainers {
    pub const SCHEMA_VERSION: u32 = 1;
}

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

/// Trim or embed-only combined-A/B repack.
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball(
    baseline_bytes: Bytes,
    experiment_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_source: &dyn MetricsSource,
    experiment_source: &dyn MetricsSource,
    manifest: &AbContainers,
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
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
}
