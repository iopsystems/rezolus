use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceExtension {
    pub service_name: String,
    #[serde(default)]
    pub service_metadata: HashMap<String, String>,
    #[serde(default)]
    pub slo: Option<serde_json::Value>,
    pub kpis: Vec<Kpi>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kpi {
    pub role: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub query: String,
    #[serde(rename = "type")]
    pub metric_type: String,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub unit_system: Option<String>,
    /// Whether the parquet file contains data for this KPI's query.
    /// Set by `rezolus parquet annotate` during validation.
    #[serde(default = "default_available")]
    pub available: bool,
}

fn default_available() -> bool {
    true
}

impl ServiceExtension {
    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    pub fn throughput_query(&self) -> Option<&str> {
        self.kpis
            .iter()
            .find(|k| k.role == "throughput")
            .map(|k| k.query.as_str())
    }
}
