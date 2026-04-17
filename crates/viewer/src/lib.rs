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

    /// Returns systeminfo JSON from parquet file metadata, or null
    pub fn systeminfo(&self) -> Option<String> {
        self.file_metadata.get("systeminfo").cloned()
    }

    /// Returns selection JSON from parquet file metadata, or null
    pub fn selection(&self) -> Option<String> {
        self.file_metadata.get("selection").cloned()
    }

    /// Returns all file-level metadata as a JSON object, mirroring the
    /// server's /file_metadata endpoint.  Values that are valid JSON are
    /// embedded as-is; everything else becomes a JSON string.
    pub fn file_metadata_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for (key, val) in &self.file_metadata {
            let json_val = serde_json::from_str(val)
                .unwrap_or_else(|_| serde_json::Value::String(val.clone()));
            map.insert(key.clone(), json_val);
        }
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
