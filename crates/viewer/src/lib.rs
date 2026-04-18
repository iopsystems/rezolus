use std::sync::Arc;

use metriken_query::{Bytes, QueryEngine, Tsdb};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct Viewer {
    engine: QueryEngine<Arc<Tsdb>>,
    file_metadata: std::collections::HashMap<String, String>,
}

#[derive(Serialize)]
struct MetadataResponse {
    status: String,
    data: MetadataData,
}

#[derive(Serialize)]
struct MetadataData {
    #[serde(rename = "minTime")]
    min_time: f64,
    #[serde(rename = "maxTime")]
    max_time: f64,
    #[serde(rename = "fileChecksum")]
    file_checksum: String,
}

#[derive(Serialize)]
struct ViewerInfo {
    interval: f64,
    source: String,
    version: String,
    filename: String,
    #[serde(rename = "minTime")]
    min_time: f64,
    #[serde(rename = "maxTime")]
    max_time: f64,
    counter_names: Vec<String>,
    gauge_names: Vec<String>,
    histogram_names: Vec<String>,
}

#[wasm_bindgen]
impl Viewer {
    #[wasm_bindgen(constructor)]
    pub fn new(data: &[u8], filename: &str) -> Result<Viewer, JsValue> {
        let bytes = Bytes::from(data.to_vec());
        let mut tsdb = Tsdb::load_from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&format!("Failed to load parquet: {}", e)))?;
        tsdb.set_filename(filename.to_string());

        let file_metadata = tsdb.file_metadata().clone();
        let engine = QueryEngine::new(Arc::new(tsdb));

        Ok(Viewer {
            engine,
            file_metadata,
        })
    }

    /// Returns JSON metadata compatible with /api/v1/metadata
    pub fn metadata(&self) -> String {
        let tsdb = self.engine.tsdb();
        let (min_time, max_time) = tsdb
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));

        serde_json::to_string(&MetadataResponse {
            status: "success".to_string(),
            data: MetadataData {
                min_time,
                max_time,
                file_checksum: String::new(),
            },
        })
        .unwrap()
    }

    /// Returns JSON with viewer info (interval, source, version, metric names)
    pub fn info(&self) -> String {
        let tsdb = self.engine.tsdb();
        let (min_time, max_time) = tsdb
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));

        serde_json::to_string(&ViewerInfo {
            interval: tsdb.interval(),
            source: tsdb.source().to_string(),
            version: tsdb.version().to_string(),
            filename: tsdb.filename().to_string(),
            min_time,
            max_time,
            counter_names: tsdb
                .counter_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            gauge_names: tsdb
                .gauge_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            histogram_names: tsdb
                .histogram_names()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .unwrap()
    }

    /// Returns systeminfo JSON from parquet file metadata.
    ///
    /// For multi-node combined files (>1 node in per_source_metadata), returns
    /// an object keyed by node name with each node's systeminfo.  For single-node
    /// files, returns the flat systeminfo string.
    pub fn systeminfo(&self) -> Option<String> {
        // Try multi-node first
        if let Some(psm_str) = self.file_metadata.get("per_source_metadata") {
            if let Ok(psm) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str) {
                if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
                    let mut nodes = serde_json::Map::new();
                    for (sub_key, entry) in rez_group {
                        let obj = match entry.as_object() {
                            Some(o) => o,
                            None => continue,
                        };
                        let sysinfo_val = match obj.get("systeminfo") {
                            Some(v) => v,
                            None => continue,
                        };
                        let node_name = obj
                            .get("node")
                            .and_then(|v| v.as_str())
                            .unwrap_or(sub_key);
                        nodes.insert(node_name.to_string(), sysinfo_val.clone());
                    }
                    if nodes.len() > 1 {
                        return serde_json::to_string(&serde_json::Value::Object(nodes)).ok();
                    }
                }
            }
        }
        // Fall back to flat systeminfo
        self.file_metadata.get("systeminfo").cloned()
    }

    /// Returns selection JSON from parquet file metadata, or null
    pub fn selection(&self) -> Option<String> {
        self.file_metadata.get("selection").cloned()
    }

    /// Returns all file-level metadata as a JSON object, mirroring the
    /// server's /file_metadata endpoint.  Values that are valid JSON are
    /// embedded as-is; everything else becomes a JSON string.
    ///
    /// Includes pre-computed `nodes`, `node_versions`, and
    /// `service_instances` fields so the frontend doesn't have to
    /// re-parse `per_source_metadata` itself.
    pub fn file_metadata_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for (key, val) in &self.file_metadata {
            let json_val = serde_json::from_str(val)
                .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
            map.insert(key.clone(), json_val);
        }
        enrich_with_multi_node_info(&mut map);
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".into())
    }

    /// Execute a PromQL range query. Returns JSON compatible with
    /// /api/v1/query_range response format.
    pub fn query_range(&self, query: &str, start: f64, end: f64, step: f64) -> String {
        match self.engine.query_range(query, start, end, step) {
            Ok(result) => {
                let json = serde_json::to_string(&result).unwrap_or_else(|e| {
                    format!(
                        r#"{{"status":"error","error":"serialization error: {}"}}"#,
                        e
                    )
                });
                format!(r#"{{"status":"success","data":{}}}"#, json)
            }
            Err(e) => {
                let msg = format!("{}", e).replace('"', "\\\"");
                format!(r#"{{"status":"error","error":"{}"}}"#, msg)
            }
        }
    }

    /// Execute a PromQL instant query.
    pub fn query(&self, query: &str, time: f64) -> String {
        match self.engine.query(query, Some(time)) {
            Ok(result) => {
                let json = serde_json::to_string(&result).unwrap_or_else(|e| {
                    format!(
                        r#"{{"status":"error","error":"serialization error: {}"}}"#,
                        e
                    )
                });
                format!(r#"{{"status":"success","data":{}}}"#, json)
            }
            Err(e) => {
                let msg = format!("{}", e).replace('"', "\\\"");
                format!(r#"{{"status":"error","error":"{}"}}"#, msg)
            }
        }
    }
}

/// Enrich a file-metadata JSON map with pre-computed multi-node info.
///
/// Parses `per_source_metadata` and adds `nodes`, `node_versions`, and
/// `service_instances` so the frontend doesn't have to duplicate this logic.
fn enrich_with_multi_node_info(map: &mut serde_json::Map<String, serde_json::Value>) {
    let psm = match map.get("per_source_metadata").and_then(|v| v.as_object()) {
        Some(psm) => psm.clone(),
        None => return,
    };

    let mut nodes = Vec::new();
    let mut node_versions = serde_json::Map::new();
    if let Some(rez_group) = psm.get("rezolus").and_then(|v| v.as_object()) {
        for (sub_key, entry) in rez_group {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
            };
            let node_name = obj
                .get("node")
                .and_then(|v| v.as_str())
                .unwrap_or(sub_key);
            if !nodes.contains(&node_name.to_string()) {
                nodes.push(node_name.to_string());
            }
            if let Some(version) = obj.get("version").and_then(|v| v.as_str()) {
                node_versions
                    .insert(node_name.to_string(), serde_json::Value::String(version.to_string()));
            }
        }
    }

    let mut service_instances = serde_json::Map::new();
    for (source, group) in &psm {
        if source == "rezolus" {
            continue;
        }
        let group_obj = match group.as_object() {
            Some(o) => o,
            None => continue,
        };
        let mut instances = Vec::new();
        for (sub_key, entry) in group_obj {
            let obj = match entry.as_object() {
                Some(o) => o,
                None => continue,
            };
            let instance_id = obj
                .get("instance")
                .and_then(|v| v.as_str())
                .unwrap_or(sub_key);
            let node = obj.get("node").and_then(|v| v.as_str());
            let mut inst = serde_json::Map::new();
            inst.insert("id".into(), serde_json::Value::String(instance_id.to_string()));
            inst.insert(
                "node".into(),
                node.map(|n| serde_json::Value::String(n.to_string()))
                    .unwrap_or(serde_json::Value::Null),
            );
            instances.push(serde_json::Value::Object(inst));
        }
        if !instances.is_empty() {
            service_instances.insert(source.clone(), serde_json::Value::Array(instances));
        }
    }

    map.insert(
        "nodes".into(),
        serde_json::Value::Array(nodes.into_iter().map(serde_json::Value::String).collect()),
    );
    if !node_versions.is_empty() {
        map.insert("node_versions".into(), serde_json::Value::Object(node_versions));
    }
    if !service_instances.is_empty() {
        map.insert(
            "service_instances".into(),
            serde_json::Value::Object(service_instances),
        );
    }
}
