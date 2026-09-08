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
    /// Set by `rezolus recording annotate` during validation.
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
                format!("histogram_quantiles({}, {})", quantiles, self.query)
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
    /// Member service names declared by this category. The runtime
    /// requires ≥2; today's bridge generator pairs exactly two captures,
    /// but extra members in the list are tolerated for forward-compat.
    /// At dashboard-gen time, every attached capture's CLI alias must
    /// appear in this list — that's the verification gate.
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
    /// — histogram_quantiles wrapping, histogram_heatmap for buckets,
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
                format!("histogram_quantiles({}, {})", quantiles, raw_query)
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
    if category.members.len() < 2 {
        return Err(format!(
            "{source}: category must have at least 2 members, got {}",
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

/// Rezolus's service extension templates, baked in at compile time.
///
/// These live inside the crate rather than at the repository root so the
/// package is self-contained: a path escaping `CARGO_MANIFEST_DIR` resolves
/// for a git dependency (cargo clones the whole repository) but not for a
/// vendored or packaged tree, where cargo copies only the package's own
/// files -- `cargo vendor` then fails with a proc-macro panic. The static
/// site symlinks to these files from `site/viewer/templates/`.
#[cfg(not(target_arch = "wasm32"))]
static EMBEDDED_TEMPLATES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/templates");

#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_TEMPLATES_DIR: &str = "crates/dashboard/templates";
#[cfg(not(target_arch = "wasm32"))]
const TEMPLATES_ENV_VAR: &str = "REZOLUS_TEMPLATES";

impl TemplateRegistry {
    /// Resolve the template directory from (in priority order):
    /// 1. Explicit CLI `--templates` path
    /// 2. `REZOLUS_TEMPLATES` environment variable
    /// 3. Default: `crates/dashboard/templates/`
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

    /// The registry of Rezolus's own service templates, baked in at compile
    /// time.
    ///
    /// This lives here rather than in the binary crate so the templates travel
    /// with `dashboard` itself: a consumer that depends on this crate (the wasm
    /// viewer, or an external tool pulling it in as a git dependency) gets the
    /// same ten services and one category the `rezolus` binary renders, with no
    /// way for the two to drift.
    ///
    /// A template that fails to parse is a build-time authoring error, not a
    /// runtime condition, so callers that want to keep going on a bad template
    /// should use [`TemplateRegistry::from_embedded`] and handle the error.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn embedded() -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_embedded(&EMBEDDED_TEMPLATES)
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

    /// Insert a service template, under its `service_name` and every alias.
    ///
    /// Lets a consumer layer templates of its own over [`embedded`] without
    /// rebuilding the registry from scratch, which is otherwise impossible:
    /// the embedded set can be read but not extended, so a downstream tool
    /// shipping one extra service had to choose between its own template and
    /// Rezolus's.
    ///
    /// Returns the keys that were displaced, in insertion order. Overwriting
    /// is the point of layering, so this is not an error -- but shadowing a
    /// Rezolus service is the specific hazard layering creates, and
    /// [`load`](Self::load) rejects a duplicate key outright, so silently
    /// dropping the collision here would make the two paths disagree about
    /// something worth knowing. An empty return means nothing was shadowed.
    ///
    /// [`embedded`]: TemplateRegistry::embedded
    #[must_use = "a non-empty result means this template shadowed an existing one"]
    pub fn insert_template(&mut self, ext: ServiceExtension) -> Vec<String> {
        let mut displaced = Vec::new();

        for key in ext
            .aliases
            .iter()
            .cloned()
            .chain(std::iter::once(ext.service_name.clone()))
        {
            if self.templates.insert(key.clone(), ext.clone()).is_some() {
                displaced.push(key);
            }
        }

        displaced
    }

    /// Insert a category into the registry's categories map. Used by the
    /// WASM viewer where categories arrive via `init_templates` rather
    /// than the disk loader. Overwrites any existing category with the
    /// same `service_name`.
    pub fn insert_category(&mut self, category: CategoryExtension) {
        self.categories
            .insert(category.service_name.clone(), category);
    }

    /// Look up a category by name (the category template's `service_name`).
    /// Returns `None` when no category with that name was loaded.
    pub fn get_category(&self, name: &str) -> Option<&CategoryExtension> {
        self.categories.get(name)
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
        // The category IS reachable by name via get_category.
        assert!(registry.get_category("inference-library").is_some());
        assert!(registry.get_category("nonexistent").is_none());
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
    fn registry_rejects_category_with_too_few_members() {
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
        assert!(err.to_string().contains("at least 2 members"), "got: {err}");
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
        assert!(registry.get_category("orphan-category").is_none());
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

    /// Every shipped template parses and is reachable by service name.
    ///
    /// `embedded()` is what external consumers get, so a template that fails to
    /// parse -- or a `templates/` file that stops being picked up because the
    /// `include_dir!` path drifted -- must fail here rather than silently
    /// serving a shorter service list than the binary does.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_embedded_templates_all_parse() {
        let registry = TemplateRegistry::embedded().expect("shipped templates must parse");

        // Named services, not just a non-empty map: these are the routes the
        // viewer offers, and losing one is a regression a count would hide.
        for service in [
            "cachecannon",
            "llm-perf",
            "sglang",
            "sglang-decode",
            "sglang-prefill",
            "sglang-router",
            "valkey",
            "vllm",
            "vllm-decode",
            "vllm-prefill",
        ] {
            assert!(
                registry.get(service).is_some(),
                "{service} template is not registered"
            );
        }

        // `inference-library` is a category, not a service, so it is indexed
        // separately -- asserting it through `get` would wrongly pass only if
        // categories leaked into the service map.
        assert!(
            registry.get_category("inference-library").is_some(),
            "the inference-library category is not registered"
        );
        assert!(
            registry.get("inference-library").is_none(),
            "a category must not be indexed as a service"
        );
    }

    /// The `include_dir!` path is relative and escapes the crate directory, so
    /// it is the kind of thing that breaks quietly. This pins it to the same
    /// directory the rest of the repo addresses.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_embedded_set_matches_the_templates_directory() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let on_disk = std::fs::read_dir(&dir)
            .expect("the crate's templates directory must exist")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count();

        assert_eq!(
            EMBEDDED_TEMPLATES
                .files()
                .filter(|f| f.path().extension().is_some_and(|x| x == "json"))
                .count(),
            on_disk,
            "the embedded set and {} have diverged",
            dir.display()
        );
    }

    /// A consumer can layer its own template over the embedded set without
    /// losing Rezolus's, which is the whole point of exposing `embedded()`.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_template_can_be_layered_over_the_embedded_set() {
        let mut registry = TemplateRegistry::embedded().expect("shipped templates must parse");

        let displaced = registry.insert_template(ServiceExtension {
            service_name: "rpc-perf".to_string(),
            aliases: vec!["rpcperf".to_string()],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: Vec::new(),
        });

        assert!(registry.get("rpc-perf").is_some(), "the added template");
        assert!(registry.get("rpcperf").is_some(), "its alias");
        assert!(
            registry.get("cachecannon").is_some(),
            "Rezolus's own templates must survive"
        );
        assert!(
            displaced.is_empty(),
            "rpc-perf shadowed nothing, got {displaced:?}"
        );
    }

    /// Shadowing an existing service is reported rather than silent.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn layering_over_an_existing_service_reports_what_it_displaced() {
        let mut registry = TemplateRegistry::embedded().expect("shipped templates must parse");

        let displaced = registry.insert_template(ServiceExtension {
            service_name: "valkey".to_string(),
            aliases: vec!["cachecannon".to_string()],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: Vec::new(),
        });

        assert_eq!(
            displaced,
            vec!["cachecannon".to_string(), "valkey".to_string()],
            "both the alias collision and the name collision are reported"
        );
    }
}
