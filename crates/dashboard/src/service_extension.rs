use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::hash_map::Entry;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceExtension {
    pub service_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
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
    /// When absent, `DEFAULT_PERCENTILES` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentiles: Option<Vec<f64>>,
    /// Whether the parquet file contains data for this KPI's query.
    /// Set by `rezolus parquet annotate` during validation.
    #[serde(default = "default_available")]
    pub available: bool,
    /// When true, this KPI's query is used as the denominator for
    /// normalized overview charts (e.g. "CPU / Throughput").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub denominator: bool,
}

fn default_available() -> bool {
    true
}

impl Kpi {
    /// Build the effective PromQL query for this KPI, wrapping histogram
    /// metrics in the appropriate histogram function.
    pub fn effective_query(&self) -> String {
        if self.metric_type == "histogram" {
            let subtype = self.subtype.as_deref().unwrap_or("percentiles");
            if subtype == "buckets" {
                format!("histogram_heatmap({})", self.query)
            } else {
                let quantiles = match &self.percentiles {
                    Some(p) => format!(
                        "[{}]",
                        p.iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None => format!(
                        "[{}]",
                        crate::DEFAULT_PERCENTILES
                            .iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                format!("histogram_percentiles({}, {})", quantiles, self.query)
            }
        } else {
            self.query.clone()
        }
    }
}

impl ServiceExtension {
    pub fn throughput_query(&self) -> Option<&str> {
        self.kpis
            .iter()
            .find(|k| k.denominator)
            .map(|k| k.query.as_str())
    }
}

/// Registry of service extension templates loaded from a directory at runtime.
///
/// Templates are indexed by `service_name` and each entry in `aliases`.
/// Constructed once at startup via [`TemplateRegistry::load`].
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    templates: HashMap<String, ServiceExtension>,
}

#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_TEMPLATES_DIR: &str = "config/templates";
#[cfg(not(target_arch = "wasm32"))]
const TEMPLATES_ENV_VAR: &str = "REZOLUS_TEMPLATES";

impl TemplateRegistry {
    /// Resolve the template directory from (in priority order):
    /// 1. Explicit CLI `--templates` path
    /// 2. `REZOLUS_TEMPLATES` environment variable
    /// 3. Default: `config/templates/`
    #[cfg(not(target_arch = "wasm32"))]
    pub fn resolve_and_load(cli_path: Option<&Path>) -> Self {
        let dir = cli_path
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var(TEMPLATES_ENV_VAR).ok().map(Into::into))
            .unwrap_or_else(|| DEFAULT_TEMPLATES_DIR.into());

        match Self::load(&dir) {
            Ok(registry) => registry,
            Err(e) => {
                eprintln!(
                    "warning: failed to load templates from {}: {e}",
                    dir.display()
                );
                Self::empty()
            }
        }
    }

    /// Scan `dir` for `*.json` files, parse each as `ServiceExtension`,
    /// and index by `service_name` and each alias.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut templates = HashMap::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "json") {
                let content = std::fs::read_to_string(&path)?;
                let ext: ServiceExtension = serde_json::from_str(&content)
                    .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

                insert_template_key(&mut templates, ext.service_name.clone(), &path, &ext)?;
                for alias in &ext.aliases {
                    insert_template_key(&mut templates, alias.clone(), &path, &ext)?;
                }
            }
        }

        Ok(Self { templates })
    }

    /// Create an empty registry (no templates).
    pub fn empty() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Create a registry from a pre-parsed list of templates.
    /// Used by the WASM viewer where templates are passed in from JS.
    pub fn from_templates(templates: Vec<ServiceExtension>) -> Self {
        let mut map = HashMap::new();
        for ext in templates {
            for alias in ext.aliases.clone() {
                map.insert(alias, ext.clone());
            }
            map.insert(ext.service_name.clone(), ext);
        }
        Self { templates: map }
    }

    /// Look up a template by service name or alias.
    pub fn get(&self, source: &str) -> Option<&ServiceExtension> {
        self.templates.get(source)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn insert_template_key(
    templates: &mut HashMap<String, ServiceExtension>,
    key: String,
    path: &Path,
    ext: &ServiceExtension,
) -> Result<(), Box<dyn std::error::Error>> {
    match templates.entry(key.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(ext.clone());
            Ok(())
        }
        Entry::Occupied(_) => {
            Err(format!("duplicate template key {:?} in {}", key, path.display()).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_template(
        dir: &tempfile::TempDir,
        name: &str,
        body: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(dir.path().join(name), body)?;
        Ok(())
    }

    #[test]
    fn load_indexes_service_name_and_aliases() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "service.json",
            r#"{
                "service_name": "valkey",
                "aliases": ["redis"],
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();

        let registry = TemplateRegistry::load(dir.path()).unwrap();

        assert_eq!(
            registry.get("valkey").map(|ext| ext.service_name.as_str()),
            Some("valkey")
        );
        assert_eq!(
            registry.get("redis").map(|ext| ext.service_name.as_str()),
            Some("valkey")
        );
    }

    #[test]
    fn load_rejects_duplicate_keys_across_templates() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "one.json",
            r#"{
                "service_name": "valkey",
                "aliases": ["redis"],
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();
        write_template(
            &dir,
            "two.json",
            r#"{
                "service_name": "redis",
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();

        let err = TemplateRegistry::load(dir.path()).unwrap_err().to_string();

        assert!(err.contains("duplicate template key"));
        assert!(err.contains("redis"));
    }
}
