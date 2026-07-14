//! The `.rez` per-sampler archive: an uncompressed tar of `manifest.json` plus
//! one `<sampler>.parquet` table per sampler. See the Stage-3 plan header for
//! the format decisions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `.rez` manifest schema version.
pub const REZ_SCHEMA_VERSION: u32 = 1;
/// Manifest filename inside the tar.
pub const REZ_MANIFEST_NAME: &str = "manifest.json";

/// Top-level `.rez` manifest (`manifest.json`).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezManifest {
    pub version: u32,
    /// File-level metadata: the existing `parquet_metadata` keys
    /// (`source`, `systeminfo`, `sampling_interval_ms`, `descriptions`, ...).
    pub metadata: BTreeMap<String, String>,
    pub tables: Vec<RezTableIndex>,
}

/// One entry in the manifest's table index.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezTableIndex {
    pub sampler: String,
    pub file: String,
    pub columns: Vec<String>,
    pub rows: u64,
    /// Observed mean row interval (ns); `None` when fewer than 2 rows.
    pub cadence_ns: Option<u64>,
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    #[test]
    fn manifest_json_round_trips() {
        let m = RezManifest {
            version: REZ_SCHEMA_VERSION,
            metadata: [("source".to_string(), "rezolus".to_string())].into_iter().collect(),
            tables: vec![RezTableIndex {
                sampler: "cpu_usage".to_string(),
                file: "cpu_usage.parquet".to_string(),
                columns: vec!["5".to_string()],
                rows: 3,
                cadence_ns: Some(1_000_000_000),
            }],
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: RezManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m, back);
        assert_eq!(REZ_MANIFEST_NAME, "manifest.json");
    }
}
