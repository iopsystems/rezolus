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
    /// Custom percentile quantiles for histogram KPIs (e.g. [0.5, 0.95]).
    /// When absent, `common::DEFAULT_PERCENTILES` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentiles: Option<Vec<f64>>,
    /// Whether the parquet file contains data for this KPI's query.
    /// Set by `rezolus parquet annotate` during validation.
    #[serde(default = "default_available")]
    pub available: bool,
}

fn default_available() -> bool {
    true
}

impl ServiceExtension {
    pub fn throughput_query(&self) -> Option<&str> {
        self.kpis
            .iter()
            .find(|k| k.role == "throughput")
            .map(|k| k.query.as_str())
    }
}
