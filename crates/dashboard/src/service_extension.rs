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
    /// Optional subgroup name within the role group. KPIs sharing a
    /// role + subgroup render inside the same subgroup; KPIs without
    /// a subgroup land in the role's default unnamed subgroup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgroup: Option<String>,
    /// Optional one-line explanation rendered under the subgroup header.
    /// Only honored on the first KPI that opens a given subgroup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgroup_description: Option<String>,
    /// When true, render this KPI as a full-width chart spanning both
    /// columns of the group's grid.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_width: bool,
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

// ─────────────────────────────────────────────────────────────────────────
// Category extension — declares that two ServiceExtensions belong to the
// same kind of system, and exposes a unified set of KPIs for compare-mode
// A/B rendering across them. See
// docs/superpowers/specs/2026-04-27-inference-library-bridge-template-design.md.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryExtension {
    pub service_name: String,
    /// Always `true` on a category file. The shared loader uses this flag
    /// to route the parsed JSON into the category map instead of services.
    #[serde(default)]
    pub category: bool,
    /// Exactly two member service names. Order is irrelevant for matching;
    /// the dashboard generator passes the live capture ordering at gen time.
    pub members: Vec<String>,
    pub kpis: Vec<CategoryKpi>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryKpi {
    pub role: String,
    pub title: String,
    #[serde(rename = "type")]
    pub metric_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percentiles: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub denominator: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgroup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgroup_description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full_width: bool,
    /// Per-member source title. When a member is omitted, the category KPI's
    /// own `title` is used as the lookup key into that member's template.
    #[serde(default)]
    pub member_titles: HashMap<String, String>,
}

impl CategoryKpi {
    /// Title to look up in the given member's template. Defaults to the
    /// category KPI's own `title` when the member is absent from
    /// `member_titles`.
    pub fn member_title<'a>(&'a self, member: &str) -> &'a str {
        self.member_titles
            .get(member)
            .map(String::as_str)
            .unwrap_or(self.title.as_str())
    }

    /// Build the same effective query string that a regular `Kpi` would
    /// produce given the supplied raw query. Mirrors `Kpi::effective_query`
    /// — histogram_percentiles wrapping, histogram_heatmap for buckets,
    /// passthrough for everything else.
    pub fn effective_query(&self, raw_query: &str) -> String {
        if self.metric_type == "histogram" {
            let subtype = self.subtype.as_deref().unwrap_or("percentiles");
            if subtype == "buckets" {
                format!("histogram_heatmap({})", raw_query)
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
                format!("histogram_percentiles({}, {})", quantiles, raw_query)
            }
        } else {
            raw_query.to_string()
        }
    }
}

// Parse a single template JSON string. Returns either a service-extension
// or a category based on the top-level `category` field.
#[cfg(not(target_arch = "wasm32"))]
fn parse_template(
    content: &str,
    source: &str,
) -> Result<ParsedTemplate, Box<dyn std::error::Error>> {
    let v: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("failed to parse {source}: {e}"))?;
    let is_category = v.get("category").and_then(|b| b.as_bool()).unwrap_or(false);
    if is_category {
        let category: CategoryExtension = serde_json::from_value(v)
            .map_err(|e| format!("failed to parse category {source}: {e}"))?;
        validate_category(&category, source)?;
        Ok(ParsedTemplate::Category(category))
    } else {
        let ext: ServiceExtension =
            serde_json::from_value(v).map_err(|e| format!("failed to parse {source}: {e}"))?;
        Ok(ParsedTemplate::Service(ext))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_category(
    category: &CategoryExtension,
    source: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if category.members.len() != 2 {
        return Err(format!(
            "{source}: category must have exactly 2 members, got {}",
            category.members.len()
        )
        .into());
    }
    let allowed: std::collections::HashSet<&str> =
        category.members.iter().map(String::as_str).collect();
    for kpi in &category.kpis {
        for key in kpi.member_titles.keys() {
            if !allowed.contains(key.as_str()) {
                return Err(format!(
                    "{source}: category KPI '{}' has member_titles key '{}' that is not in members {:?}",
                    kpi.title, key, category.members,
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn finalize_categories(
    candidates: Vec<CategoryExtension>,
    services: &HashMap<String, ServiceExtension>,
) -> HashMap<String, CategoryExtension> {
    let mut out = HashMap::new();
    for category in candidates {
        let missing: Vec<&String> = category
            .members
            .iter()
            .filter(|m| !services.contains_key(m.as_str()))
            .collect();
        if !missing.is_empty() {
            eprintln!(
                "warning: dropping category '{}' — unknown member template(s): {:?}",
                category.service_name, missing
            );
            continue;
        }
        out.insert(category.service_name.clone(), category);
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
enum ParsedTemplate {
    Service(ServiceExtension),
    Category(CategoryExtension),
}

/// Registry of service extension templates loaded from a directory at runtime.
///
/// Templates are indexed by `service_name` and each entry in `aliases`.
/// Constructed once at startup via [`TemplateRegistry::load`].
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    templates: HashMap<String, ServiceExtension>,
    categories: HashMap<String, CategoryExtension>,
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

    /// Parse every `*.json` file in an embedded `include_dir::Dir` as
    /// `ServiceExtension` and index them. Used in release builds where
    /// the templates are baked into the binary via `include_dir!`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_embedded(dir: &include_dir::Dir<'_>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut templates = HashMap::new();
        let mut category_candidates: Vec<CategoryExtension> = Vec::new();
        for file in dir.files() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let content = file
                .contents_utf8()
                .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
            match parse_template(content, &path.display().to_string())? {
                ParsedTemplate::Service(ext) => {
                    insert_template_key(&mut templates, ext.service_name.clone(), path, &ext)?;
                    for alias in &ext.aliases {
                        insert_template_key(&mut templates, alias.clone(), path, &ext)?;
                    }
                }
                ParsedTemplate::Category(category) => {
                    category_candidates.push(category);
                }
            }
        }
        let categories = finalize_categories(category_candidates, &templates);
        Ok(Self {
            templates,
            categories,
        })
    }

    /// Scan `dir` for `*.json` files, parse each as `ServiceExtension`,
    /// and index by `service_name` and each alias.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut templates = HashMap::new();
        let mut category_candidates: Vec<CategoryExtension> = Vec::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            match parse_template(&content, &path.display().to_string())? {
                ParsedTemplate::Service(ext) => {
                    insert_template_key(&mut templates, ext.service_name.clone(), &path, &ext)?;
                    for alias in &ext.aliases {
                        insert_template_key(&mut templates, alias.clone(), &path, &ext)?;
                    }
                }
                ParsedTemplate::Category(category) => {
                    category_candidates.push(category);
                }
            }
        }

        let categories = finalize_categories(category_candidates, &templates);
        Ok(Self {
            templates,
            categories,
        })
    }

    /// Create an empty registry (no templates).
    pub fn empty() -> Self {
        Self {
            templates: HashMap::new(),
            categories: HashMap::new(),
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
        Self {
            templates: map,
            categories: HashMap::new(),
        }
    }

    /// Look up a template by service name or alias.
    pub fn get(&self, source: &str) -> Option<&ServiceExtension> {
        self.templates.get(source)
    }

    /// Insert a category into the registry's categories map. Used by the
    /// WASM viewer where categories arrive via `init_templates` rather
    /// than the disk loader. Overwrites any existing category with the
    /// same `service_name`.
    pub fn insert_category(&mut self, category: CategoryExtension) {
        self.categories
            .insert(category.service_name.clone(), category);
    }

    /// Look up a category whose `members` set equals `{member_a, member_b}`
    /// (order-insensitive). Returns `None` when no matching category exists.
    pub fn find_category(&self, member_a: &str, member_b: &str) -> Option<&CategoryExtension> {
        self.categories.values().find(|b| {
            b.members.len() == 2
                && ((b.members[0] == member_a && b.members[1] == member_b)
                    || (b.members[0] == member_b && b.members[1] == member_a))
        })
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

    #[test]
    fn registry_loads_service_and_category_separately() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "vllm.json",
            r#"{
                "service_name": "vllm",
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();
        write_template(
            &dir,
            "sglang.json",
            r#"{
                "service_name": "sglang",
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();
        write_template(
            &dir,
            "inference-library.json",
            r#"{
                "service_name": "inference-library",
                "category": true,
                "members": ["vllm", "sglang"],
                "kpis": []
            }"#,
        )
        .unwrap();

        let registry = TemplateRegistry::load(dir.path()).unwrap();

        // Service templates remain accessible via `get`.
        assert!(registry.get("vllm").is_some());
        assert!(registry.get("sglang").is_some());
        // Category files do NOT pollute the service map.
        assert!(registry.get("inference-library").is_none());
        // The category IS reachable via find_category in either order.
        assert!(registry.find_category("vllm", "sglang").is_some());
        assert!(registry.find_category("sglang", "vllm").is_some());
        assert!(registry.find_category("vllm", "valkey").is_none());
    }

    #[test]
    fn parses_category_extension_json() {
        let json = r#"{
            "service_name": "inference-library",
            "category": true,
            "members": ["vllm", "sglang"],
            "kpis": [
                {
                    "role": "throughput",
                    "title": "Generation Token Rate",
                    "type": "delta_counter",
                    "unit_system": "rate",
                    "denominator": true,
                    "member_titles": {
                        "vllm":   "Generation Token Rate",
                        "sglang": "Generation Token Rate"
                    }
                }
            ]
        }"#;
        let category: CategoryExtension = serde_json::from_str(json).expect("parse");
        assert_eq!(category.service_name, "inference-library");
        assert_eq!(category.members, ["vllm".to_string(), "sglang".to_string()]);
        assert_eq!(category.kpis.len(), 1);
        let k = &category.kpis[0];
        assert_eq!(k.title, "Generation Token Rate");
        assert_eq!(k.metric_type, "delta_counter");
        assert!(k.denominator);
        assert_eq!(
            k.member_titles.get("vllm").map(String::as_str),
            Some("Generation Token Rate"),
        );
    }

    #[test]
    fn registry_rejects_category_with_wrong_member_count() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "bad.json",
            r#"{
                "service_name": "broken-category",
                "category": true,
                "members": ["only-one"],
                "kpis": []
            }"#,
        )
        .unwrap();
        let err = TemplateRegistry::load(dir.path()).expect_err("should reject");
        assert!(err.to_string().contains("exactly 2 members"), "got: {err}");
    }

    #[test]
    fn registry_drops_category_when_member_template_missing() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "vllm.json",
            r#"{
                "service_name": "vllm",
                "service_metadata": {},
                "slo": null,
                "kpis": []
            }"#,
        )
        .unwrap();
        write_template(
            &dir,
            "orphan-category.json",
            r#"{
                "service_name": "orphan-category",
                "category": true,
                "members": ["vllm", "tensorrt-llm"],
                "kpis": []
            }"#,
        )
        .unwrap();

        let registry = TemplateRegistry::load(dir.path()).unwrap();

        // The category dropped silently because tensorrt-llm isn't loaded.
        assert!(registry.find_category("vllm", "tensorrt-llm").is_none());
    }

    #[test]
    fn registry_rejects_category_with_unknown_member_titles_key() {
        let dir = tempfile::tempdir().unwrap();
        write_template(
            &dir,
            "bad.json",
            r#"{
                "service_name": "broken-category",
                "category": true,
                "members": ["vllm", "sglang"],
                "kpis": [
                    {
                        "role": "throughput",
                        "title": "X",
                        "type": "delta_counter",
                        "member_titles": { "tensorrt": "X" }
                    }
                ]
            }"#,
        )
        .unwrap();
        let err = TemplateRegistry::load(dir.path()).expect_err("should reject");
        let msg = err.to_string();
        assert!(
            msg.contains("member_titles") && msg.contains("tensorrt"),
            "got: {msg}",
        );
    }
}
