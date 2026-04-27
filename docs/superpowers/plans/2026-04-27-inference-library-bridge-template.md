# Inference Library Bridge Template Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "service bridge" template type that joins two existing service templates, producing a single `/service/<bridge>` section in compare mode where each plot's baseline and experiment fetch each consult the corresponding member template's query.

**Architecture:** Bridge files (`bridge: true`, two `members`, KPIs with `member_titles`) load into a separate map on `TemplateRegistry`. When compare mode attaches two captures whose sources match a bridge's members, the dashboard generator emits one bridge section instead of two per-service sections. Each bridge plot ships with both `promql_query` (baseline) and a new optional `promql_query_experiment` field (experiment); `CompareChartWrapper` already does its own per-capture fetch, so it just learns to prefer `promql_query_experiment` when set.

**Tech Stack:** Rust (`crates/dashboard`, `crates/viewer`, `src/viewer`), JS (Mithril `src/viewer/assets/lib`), JSON config under `config/templates`.

**Reference spec:** [`docs/superpowers/specs/2026-04-27-inference-library-bridge-template-design.md`](../specs/2026-04-27-inference-library-bridge-template-design.md)

---

### Task 1: Define `BridgeExtension` and `BridgeKpi` types with JSON parsing

**Files:**
- Modify: `crates/dashboard/src/service_extension.rs` (add types near the existing `ServiceExtension` block)

- [ ] **Step 1: Write the failing test**

Append to the existing `mod tests` block in `crates/dashboard/src/service_extension.rs`:

```rust
#[test]
fn parses_bridge_extension_json() {
    let json = r#"{
        "service_name": "inference-library",
        "bridge": true,
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
    let bridge: BridgeExtension = serde_json::from_str(json).expect("parse");
    assert_eq!(bridge.service_name, "inference-library");
    assert_eq!(bridge.members, ["vllm".to_string(), "sglang".to_string()]);
    assert_eq!(bridge.kpis.len(), 1);
    let k = &bridge.kpis[0];
    assert_eq!(k.title, "Generation Token Rate");
    assert_eq!(k.metric_type, "delta_counter");
    assert!(k.denominator);
    assert_eq!(
        k.member_titles.get("vllm").map(String::as_str),
        Some("Generation Token Rate"),
    );
}
```

- [ ] **Step 2: Run test — expect compile failure (`BridgeExtension` undefined)**

```bash
cargo test -p dashboard parses_bridge_extension_json 2>&1 | tail -10
```

Expected: `error[E0433]: ... could not find BridgeExtension`

- [ ] **Step 3: Add the types**

Add to `crates/dashboard/src/service_extension.rs`, right after the `ServiceExtension` `impl` block (around line 105):

```rust
// ─────────────────────────────────────────────────────────────────────────
// Bridge extension — ties two ServiceExtensions together for compare-mode
// A/B rendering across heterogeneous services. See
// docs/superpowers/specs/2026-04-27-inference-library-bridge-template-design.md.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeExtension {
    pub service_name: String,
    /// Always `true` on a bridge file. The shared loader uses this flag
    /// to route the parsed JSON into the bridge map instead of services.
    #[serde(default)]
    pub bridge: bool,
    /// Exactly two member service names. Order is irrelevant for matching;
    /// the dashboard generator passes the live capture ordering at gen time.
    pub members: Vec<String>,
    pub kpis: Vec<BridgeKpi>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeKpi {
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
    /// Per-member source title. When a member is omitted, the bridge KPI's
    /// own `title` is used as the lookup key into that member's template.
    #[serde(default)]
    pub member_titles: HashMap<String, String>,
}

impl BridgeKpi {
    /// Title to look up in the given member's template. Defaults to the
    /// bridge KPI's own `title` when the member is absent from
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
                        p.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
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
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p dashboard parses_bridge_extension_json
```

Expected: `test parses_bridge_extension_json ... ok`

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/service_extension.rs
git commit -m "feat(dashboard): add BridgeExtension + BridgeKpi types"
```

---

### Task 2: Extend `TemplateRegistry` to load bridges into a separate map

**Files:**
- Modify: `crates/dashboard/src/service_extension.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn registry_loads_service_and_bridge_separately() {
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
            "bridge": true,
            "members": ["vllm", "sglang"],
            "kpis": []
        }"#,
    )
    .unwrap();

    let registry = TemplateRegistry::load(dir.path()).unwrap();

    // Service templates remain accessible via `get`.
    assert!(registry.get("vllm").is_some());
    assert!(registry.get("sglang").is_some());
    // Bridge files do NOT pollute the service map.
    assert!(registry.get("inference-library").is_none());
    // The bridge IS reachable via find_bridge in either order.
    assert!(registry.find_bridge("vllm", "sglang").is_some());
    assert!(registry.find_bridge("sglang", "vllm").is_some());
    assert!(registry.find_bridge("vllm", "valkey").is_none());
}
```

- [ ] **Step 2: Run test — expect compile/method failure**

```bash
cargo test -p dashboard registry_loads_service_and_bridge_separately 2>&1 | tail -10
```

Expected: error: `find_bridge` not found, or test fails because bridge ends up in `templates`.

- [ ] **Step 3: Update the registry to recognize and route bridge files**

In `crates/dashboard/src/service_extension.rs`, change the `TemplateRegistry` struct (around line 112):

```rust
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    templates: HashMap<String, ServiceExtension>,
    bridges: HashMap<String, BridgeExtension>,
}
```

Update `Self::empty()` (search for `pub fn empty()`):

```rust
pub fn empty() -> Self {
    Self {
        templates: HashMap::new(),
        bridges: HashMap::new(),
    }
}
```

Update `from_templates` (around line 195):

```rust
pub fn from_templates(templates: Vec<ServiceExtension>) -> Self {
    let mut map = HashMap::new();
    for ext in templates {
        for alias in ext.aliases.clone() {
            map.insert(alias, ext.clone());
        }
        map.insert(ext.service_name.clone(), ext);
    }
    Self { templates: map, bridges: HashMap::new() }
}
```

Add a `find_bridge` method on `TemplateRegistry` (right after `get`):

```rust
/// Look up a bridge whose `members` set equals `{member_a, member_b}`
/// (order-insensitive). Returns `None` when no matching bridge exists.
pub fn find_bridge(&self, member_a: &str, member_b: &str) -> Option<&BridgeExtension> {
    self.bridges.values().find(|b| {
        b.members.len() == 2
            && ((b.members[0] == member_a && b.members[1] == member_b)
                || (b.members[0] == member_b && b.members[1] == member_a))
    })
}
```

Replace the body of `from_embedded` (around line 149) and `load` (around line 173) so each routes by the `bridge` flag. The two functions parse the same JSON shape, so introduce a helper:

```rust
// Parse a single template JSON string. Returns either a service-extension
// or a bridge based on the top-level `bridge` field. Lives next to the
// other private helpers near the top of the impl block.
fn parse_template(content: &str, source: &str) -> Result<ParsedTemplate, Box<dyn std::error::Error>> {
    let v: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("failed to parse {source}: {e}"))?;
    let is_bridge = v.get("bridge").and_then(|b| b.as_bool()).unwrap_or(false);
    if is_bridge {
        let bridge: BridgeExtension = serde_json::from_value(v)
            .map_err(|e| format!("failed to parse bridge {source}: {e}"))?;
        Ok(ParsedTemplate::Bridge(bridge))
    } else {
        let ext: ServiceExtension = serde_json::from_value(v)
            .map_err(|e| format!("failed to parse {source}: {e}"))?;
        Ok(ParsedTemplate::Service(ext))
    }
}

enum ParsedTemplate {
    Service(ServiceExtension),
    Bridge(BridgeExtension),
}
```

Place these BEFORE `impl TemplateRegistry` so they're in module scope.

Now rewrite `load` (the disk path):

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn load(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
    let mut templates = HashMap::new();
    let mut bridges = HashMap::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::empty()),
        Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "json") {
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
            ParsedTemplate::Bridge(bridge) => {
                bridges.insert(bridge.service_name.clone(), bridge);
            }
        }
    }

    Ok(Self { templates, bridges })
}
```

Same shape for `from_embedded`:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn from_embedded(dir: &include_dir::Dir<'_>) -> Result<Self, Box<dyn std::error::Error>> {
    let mut templates = HashMap::new();
    let mut bridges = HashMap::new();
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
            ParsedTemplate::Bridge(bridge) => {
                bridges.insert(bridge.service_name.clone(), bridge);
            }
        }
    }
    Ok(Self { templates, bridges })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p dashboard registry_loads_service_and_bridge_separately
```

Expected: `test registry_loads_service_and_bridge_separately ... ok`. Also re-run the existing tests to make sure nothing else regressed:

```bash
cargo test -p dashboard
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/service_extension.rs
git commit -m "feat(dashboard): TemplateRegistry routes bridge files into a separate map"
```

---

### Task 3: Reject malformed bridge files at load time

**Files:**
- Modify: `crates/dashboard/src/service_extension.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn registry_rejects_bridge_with_wrong_member_count() {
    let dir = tempfile::tempdir().unwrap();
    write_template(
        &dir,
        "bad.json",
        r#"{
            "service_name": "broken-bridge",
            "bridge": true,
            "members": ["only-one"],
            "kpis": []
        }"#,
    )
    .unwrap();
    let err = TemplateRegistry::load(dir.path()).expect_err("should reject");
    assert!(err.to_string().contains("exactly 2 members"), "got: {err}");
}

#[test]
fn registry_rejects_bridge_with_unknown_member_titles_key() {
    let dir = tempfile::tempdir().unwrap();
    write_template(
        &dir,
        "bad.json",
        r#"{
            "service_name": "broken-bridge",
            "bridge": true,
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
```

- [ ] **Step 2: Run tests — expect failure (no validation yet)**

```bash
cargo test -p dashboard registry_rejects_bridge 2>&1 | tail -15
```

Expected: both tests fail with "should reject" panics — load currently accepts the malformed input.

- [ ] **Step 3: Add validation inside `parse_template`**

Replace the `parse_template` helper from Task 2 with:

```rust
fn parse_template(content: &str, source: &str) -> Result<ParsedTemplate, Box<dyn std::error::Error>> {
    let v: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| format!("failed to parse {source}: {e}"))?;
    let is_bridge = v.get("bridge").and_then(|b| b.as_bool()).unwrap_or(false);
    if is_bridge {
        let bridge: BridgeExtension = serde_json::from_value(v)
            .map_err(|e| format!("failed to parse bridge {source}: {e}"))?;
        validate_bridge(&bridge, source)?;
        Ok(ParsedTemplate::Bridge(bridge))
    } else {
        let ext: ServiceExtension = serde_json::from_value(v)
            .map_err(|e| format!("failed to parse {source}: {e}"))?;
        Ok(ParsedTemplate::Service(ext))
    }
}

fn validate_bridge(bridge: &BridgeExtension, source: &str) -> Result<(), Box<dyn std::error::Error>> {
    if bridge.members.len() != 2 {
        return Err(format!(
            "{source}: bridge must have exactly 2 members, got {}",
            bridge.members.len()
        )
        .into());
    }
    let allowed: std::collections::HashSet<&str> =
        bridge.members.iter().map(String::as_str).collect();
    for kpi in &bridge.kpis {
        for key in kpi.member_titles.keys() {
            if !allowed.contains(key.as_str()) {
                return Err(format!(
                    "{source}: bridge KPI '{}' has member_titles key '{}' that is not in members {:?}",
                    kpi.title, key, bridge.members,
                )
                .into());
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify**

```bash
cargo test -p dashboard registry_rejects_bridge
cargo test -p dashboard
```

Expected: rejection tests pass; everything else still green.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/service_extension.rs
git commit -m "feat(dashboard): validate bridge member count + member_titles keys"
```

---

### Task 3.5: Drop bridges whose members aren't in the registry

**Files:**
- Modify: `crates/dashboard/src/service_extension.rs`

The spec says bridges referencing unknown service templates should be dropped (with a warning) rather than activate against missing members. Since templates and bridges are loaded in the same directory pass, we collect bridges into a temporary vec first, then validate after all services are known.

- [ ] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
#[test]
fn registry_drops_bridge_when_member_template_missing() {
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
        "orphan-bridge.json",
        r#"{
            "service_name": "orphan-bridge",
            "bridge": true,
            "members": ["vllm", "tensorrt-llm"],
            "kpis": []
        }"#,
    )
    .unwrap();

    let registry = TemplateRegistry::load(dir.path()).unwrap();

    // The bridge dropped silently because tensorrt-llm isn't loaded.
    assert!(registry.find_bridge("vllm", "tensorrt-llm").is_none());
}
```

- [ ] **Step 2: Run test — expect failure (currently the bridge IS inserted)**

```bash
cargo test -p dashboard registry_drops_bridge_when_member_template_missing 2>&1 | tail -10
```

- [ ] **Step 3: Two-pass `load` (and `from_embedded`)**

Replace the body of `load` in `crates/dashboard/src/service_extension.rs` so bridges land in a staging vec until services are fully known:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn load(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
    let mut templates = HashMap::new();
    let mut bridge_candidates: Vec<BridgeExtension> = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::empty()),
        Err(e) => return Err(format!("{}: {e}", dir.display()).into()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "json") {
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
            ParsedTemplate::Bridge(bridge) => {
                bridge_candidates.push(bridge);
            }
        }
    }

    let bridges = finalize_bridges(bridge_candidates, &templates);
    Ok(Self { templates, bridges })
}
```

Same shape for `from_embedded`:

```rust
#[cfg(not(target_arch = "wasm32"))]
pub fn from_embedded(dir: &include_dir::Dir<'_>) -> Result<Self, Box<dyn std::error::Error>> {
    let mut templates = HashMap::new();
    let mut bridge_candidates: Vec<BridgeExtension> = Vec::new();
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
            ParsedTemplate::Bridge(bridge) => {
                bridge_candidates.push(bridge);
            }
        }
    }
    let bridges = finalize_bridges(bridge_candidates, &templates);
    Ok(Self { templates, bridges })
}
```

Add the `finalize_bridges` helper next to `parse_template` / `validate_bridge`:

```rust
fn finalize_bridges(
    candidates: Vec<BridgeExtension>,
    services: &HashMap<String, ServiceExtension>,
) -> HashMap<String, BridgeExtension> {
    let mut out = HashMap::new();
    for bridge in candidates {
        let missing: Vec<&String> = bridge
            .members
            .iter()
            .filter(|m| !services.contains_key(m.as_str()))
            .collect();
        if !missing.is_empty() {
            eprintln!(
                "warning: dropping bridge '{}' — unknown member template(s): {:?}",
                bridge.service_name, missing
            );
            continue;
        }
        out.insert(bridge.service_name.clone(), bridge);
    }
    out
}
```

- [ ] **Step 4: Run test to verify**

```bash
cargo test -p dashboard registry_drops_bridge_when_member_template_missing
cargo test -p dashboard
```

Expected: green; existing tests still pass (the previous bridge tests provide both members so the bridge isn't dropped).

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/service_extension.rs
git commit -m "feat(dashboard): drop bridge when member template missing"
```

---

### Task 4: Add `promql_query_experiment` field to `Plot`

**Files:**
- Modify: `crates/dashboard/src/plot.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/dashboard/src/plot.rs` (add a `mod tests` block at the bottom if absent — check first):

```rust
#[cfg(test)]
mod plot_serialize_tests {
    use super::*;

    #[test]
    fn plot_promql_query_experiment_round_trips() {
        let mut sg = SubGroup::default();
        sg.plot_promql(
            PlotOpts::counter("X", "kpi-x", Unit::Count),
            "metric_a".to_string(),
        );
        // Mutate the just-pushed plot to set the experiment query, then
        // serialize and confirm it appears in the JSON.
        let plot = sg.plots.last_mut().unwrap();
        plot.promql_query_experiment = Some("metric_b".to_string());
        let json = serde_json::to_string(plot).unwrap();
        assert!(json.contains("\"promql_query_experiment\":\"metric_b\""));

        // Default (None) is omitted from the JSON.
        plot.promql_query_experiment = None;
        let json = serde_json::to_string(plot).unwrap();
        assert!(!json.contains("promql_query_experiment"), "got {json}");
    }
}
```

- [ ] **Step 2: Run test — expect compile failure**

```bash
cargo test -p dashboard plot_promql_query_experiment 2>&1 | tail -10
```

Expected: `error[E0560]: struct Plot has no field named promql_query_experiment` (or similar).

- [ ] **Step 3: Add the field**

In `crates/dashboard/src/plot.rs`, find the `pub struct Plot` definition (around line 253). Add after the existing `promql_query` field:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub promql_query_experiment: Option<String>,
```

Update the `plot_promql_with_descriptions` constructor (search for `self.plots.push(Plot {`, around line 214) so the new field is initialized:

```rust
self.plots.push(Plot {
    opts,
    data: Vec::new(),
    min_value: None,
    max_value: None,
    time_data: None,
    formatted_time_data: None,
    series_names: None,
    promql_query: Some(promql_query),
    promql_query_experiment: None,
    width: PlotWidth::default(),
});
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p dashboard plot_promql_query_experiment
cargo test -p dashboard
```

Expected: both green.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/plot.rs
git commit -m "feat(dashboard): add Plot.promql_query_experiment field"
```

---

### Task 5: Add `bridge::generate` (happy path — both members have all KPIs)

**Files:**
- Create: `crates/dashboard/src/dashboard/bridge.rs`
- Modify: `crates/dashboard/src/dashboard/mod.rs` (declare module)

- [ ] **Step 1: Declare the module**

In `crates/dashboard/src/dashboard/mod.rs`, add to the `mod` list (alphabetically):

```rust
mod blockio;
mod bridge;
mod cgroups;
```

- [ ] **Step 2: Write the failing test**

Create `crates/dashboard/src/dashboard/bridge.rs` with just a `mod tests` skeleton plus a placeholder `pub fn generate` so the next test compiles. Use this content for the file:

```rust
use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::{BridgeExtension, ServiceExtension};

pub fn generate(
    _data: &Tsdb,
    _all_sections: Vec<Section>,
    _bridge: &BridgeExtension,
    _baseline_member: &str,
    _baseline_ext: &ServiceExtension,
    _experiment_member: &str,
    _experiment_ext: &ServiceExtension,
) -> View {
    todo!("implement in step 4")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_extension::{BridgeKpi, Kpi};
    use std::collections::HashMap;

    fn kpi(role: &str, title: &str, query: &str) -> Kpi {
        Kpi {
            role: role.to_string(),
            title: title.to_string(),
            description: None,
            query: query.to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
        }
    }

    fn ext(name: &str, kpis: Vec<Kpi>) -> ServiceExtension {
        ServiceExtension {
            service_name: name.to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis,
        }
    }

    #[test]
    fn bridge_generate_emits_section_with_paired_queries() {
        let bridge = BridgeExtension {
            service_name: "inference-library".to_string(),
            bridge: true,
            members: vec!["vllm".to_string(), "sglang".to_string()],
            kpis: vec![BridgeKpi {
                role: "throughput".to_string(),
                title: "Generation Token Rate".to_string(),
                metric_type: "delta_counter".to_string(),
                subtype: None,
                unit_system: Some("rate".to_string()),
                percentiles: None,
                denominator: true,
                subgroup: None,
                subgroup_description: None,
                full_width: false,
                member_titles: HashMap::new(),
            }],
        };

        let vllm_ext = ext(
            "vllm",
            vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
        );
        let sglang_ext = ext(
            "sglang",
            vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
        );

        let data = Tsdb::default();
        let view = generate(
            &data,
            vec![],
            &bridge,
            "vllm",
            &vllm_ext,
            "sglang",
            &sglang_ext,
        );

        let json = serde_json::to_value(&view).unwrap();
        let groups = json
            .get("groups")
            .and_then(|g| g.as_array())
            .expect("has groups");
        assert_eq!(groups.len(), 1);
        let plots = groups[0]
            .get("subgroups")
            .and_then(|s| s.as_array())
            .and_then(|s| s.first())
            .and_then(|sg| sg.get("plots"))
            .and_then(|p| p.as_array())
            .expect("has plots");
        assert_eq!(plots.len(), 1);
        let plot = &plots[0];
        assert_eq!(plot["promql_query"].as_str(), Some("vllm_q"));
        assert_eq!(plot["promql_query_experiment"].as_str(), Some("sglang_q"));
        assert_eq!(plot["opts"]["title"].as_str(), Some("Generation Token Rate"));
    }
}
```

- [ ] **Step 3: Run test — expect panic from `todo!()`**

```bash
cargo test -p dashboard bridge_generate_emits_section_with_paired_queries 2>&1 | tail -10
```

Expected: `panicked at 'not yet implemented'`.

- [ ] **Step 4: Expose `slug` and `capitalize` from `service.rs`**

In `crates/dashboard/src/dashboard/service.rs`, find the existing `fn slug` (line 106) and `fn capitalize` (line 118). Change their visibility:

```rust
pub(crate) fn slug(s: &str) -> String { ... }
pub(crate) fn capitalize(s: &str) -> String { ... }
```

(Just add `pub(crate)` to each `fn` line; bodies are unchanged.)

- [ ] **Step 5: Add `plots_mut_last` on `SubGroup`**

In `crates/dashboard/src/plot.rs`, find `impl SubGroup` (around line 181) and add inside the impl block:

```rust
/// Mutable access to the most recently pushed plot. Used by callers
/// that mutate per-plot fields (e.g. `promql_query_experiment` on the
/// bridge generator) right after `plot_promql*`.
pub fn plots_mut_last(&mut self) -> Option<&mut Plot> {
    self.plots.last_mut()
}
```

- [ ] **Step 6: Implement `generate`**

Replace the `pub fn generate` body in `crates/dashboard/src/dashboard/bridge.rs`:

```rust
pub fn generate(
    data: &Tsdb,
    all_sections: Vec<Section>,
    bridge: &BridgeExtension,
    baseline_member: &str,
    baseline_ext: &ServiceExtension,
    experiment_member: &str,
    experiment_ext: &ServiceExtension,
) -> View {
    let mut view = View::new(data, all_sections);

    view.metadata.insert(
        "service_name".to_string(),
        serde_json::Value::String(bridge.service_name.clone()),
    );
    view.metadata.insert(
        "bridge_members".to_string(),
        serde_json::Value::Array(vec![
            serde_json::Value::String(baseline_member.to_string()),
            serde_json::Value::String(experiment_member.to_string()),
        ]),
    );

    let mut groups: Vec<(String, Group)> = Vec::new();

    for kpi in &bridge.kpis {
        let baseline_title = kpi.member_title(baseline_member);
        let experiment_title = kpi.member_title(experiment_member);
        let baseline_kpi = baseline_ext
            .kpis
            .iter()
            .find(|k| k.title == baseline_title);
        let experiment_kpi = experiment_ext
            .kpis
            .iter()
            .find(|k| k.title == experiment_title);
        let (Some(baseline_kpi), Some(experiment_kpi)) = (baseline_kpi, experiment_kpi)
        else {
            continue;
        };

        let plot_id = format!(
            "kpi-{}-{}",
            kpi.role,
            crate::dashboard::service::slug(&kpi.title)
        );

        let group = match groups.iter_mut().find(|(r, _)| *r == kpi.role) {
            Some((_, g)) => g,
            None => {
                groups.push((
                    kpi.role.clone(),
                    Group::new(
                        crate::dashboard::service::capitalize(&kpi.role),
                        format!("kpi-{}", kpi.role),
                    ),
                ));
                &mut groups.last_mut().unwrap().1
            }
        };

        let opts = match kpi.metric_type.as_str() {
            "gauge" => PlotOpts::gauge(&kpi.title, &plot_id, Unit::Count),
            "histogram" => PlotOpts::histogram(
                &kpi.title,
                &plot_id,
                Unit::Count,
                kpi.subtype.as_deref().unwrap_or("percentiles"),
            ),
            _ => PlotOpts::counter(&kpi.title, &plot_id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
        let opts = match &kpi.percentiles {
            Some(p) => opts.with_percentiles(p.clone()),
            None => opts,
        };

        let sg = match kpi.subgroup.as_deref() {
            Some(name) => {
                if group.find_subgroup(name).is_none() {
                    let new_sg = group.subgroup(name);
                    if let Some(desc) = kpi.subgroup_description.as_deref() {
                        new_sg.describe(desc);
                    }
                    new_sg
                } else {
                    group.find_subgroup(name).unwrap()
                }
            }
            None => group.default_subgroup(),
        };

        let baseline_query = kpi.effective_query(&baseline_kpi.query);
        let experiment_query = kpi.effective_query(&experiment_kpi.query);
        if kpi.full_width {
            sg.plot_promql_full(opts, baseline_query.clone());
        } else {
            sg.plot_promql(opts, baseline_query.clone());
        }
        if experiment_query != baseline_query {
            if let Some(plot) = sg.plots_mut_last() {
                plot.promql_query_experiment = Some(experiment_query);
            }
        }
    }

    for (_, group) in groups {
        view.group(group);
    }
    view
}
```

- [ ] **Step 7: Run test to verify it passes**

```bash
cargo test -p dashboard bridge_generate_emits_section_with_paired_queries
cargo test -p dashboard
```

Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/dashboard/src/dashboard/bridge.rs \
        crates/dashboard/src/dashboard/mod.rs \
        crates/dashboard/src/dashboard/service.rs \
        crates/dashboard/src/plot.rs
git commit -m "feat(dashboard): bridge::generate — paired queries per capture"
```

---

### Task 6: `bridge::generate` records unavailable bridge KPIs in section metadata

**Files:**
- Modify: `crates/dashboard/src/dashboard/bridge.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `bridge.rs`:

```rust
#[test]
fn bridge_generate_records_unavailable_when_member_lookup_misses() {
    let bridge = BridgeExtension {
        service_name: "ifx".to_string(),
        bridge: true,
        members: vec!["a".to_string(), "b".to_string()],
        kpis: vec![BridgeKpi {
            role: "throughput".to_string(),
            title: "Token Rate".to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
            member_titles: HashMap::new(),
        }],
    };
    let a = ext("a", vec![kpi("throughput", "Token Rate", "a_q")]);
    let b = ext("b", vec![]); // missing the bridged title

    let view = generate(&Tsdb::default(), vec![], &bridge, "a", &a, "b", &b);
    let json = serde_json::to_value(&view).unwrap();

    let unavailable = json
        .get("metadata")
        .and_then(|m| m.get("bridge_unavailable"))
        .and_then(|v| v.as_array())
        .expect("bridge_unavailable present");
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0]["title"].as_str(), Some("Token Rate"));
    assert_eq!(unavailable[0]["missing_member"].as_str(), Some("b"));

    // No groups were emitted (the only KPI was skipped).
    let groups = json.get("groups").and_then(|g| g.as_array()).unwrap();
    assert!(groups.is_empty());
}
```

- [ ] **Step 2: Run test — expect failure (no `bridge_unavailable` recorded)**

```bash
cargo test -p dashboard bridge_generate_records_unavailable_when_member_lookup_misses 2>&1 | tail -10
```

- [ ] **Step 3: Update `generate` to record skips**

Replace the entire body of `pub fn generate` in `crates/dashboard/src/dashboard/bridge.rs` with this complete version (it folds the Task-5 body and the new unavailable tracking together):

```rust
pub fn generate(
    data: &Tsdb,
    all_sections: Vec<Section>,
    bridge: &BridgeExtension,
    baseline_member: &str,
    baseline_ext: &ServiceExtension,
    experiment_member: &str,
    experiment_ext: &ServiceExtension,
) -> View {
    let mut view = View::new(data, all_sections);

    view.metadata.insert(
        "service_name".to_string(),
        serde_json::Value::String(bridge.service_name.clone()),
    );
    view.metadata.insert(
        "bridge_members".to_string(),
        serde_json::Value::Array(vec![
            serde_json::Value::String(baseline_member.to_string()),
            serde_json::Value::String(experiment_member.to_string()),
        ]),
    );

    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut unavailable: Vec<serde_json::Value> = Vec::new();

    for kpi in &bridge.kpis {
        let baseline_title = kpi.member_title(baseline_member);
        let experiment_title = kpi.member_title(experiment_member);
        let baseline_kpi = baseline_ext.kpis.iter().find(|k| k.title == baseline_title);
        let experiment_kpi = experiment_ext
            .kpis
            .iter()
            .find(|k| k.title == experiment_title);

        let (baseline_kpi, experiment_kpi) = match (baseline_kpi, experiment_kpi) {
            (Some(a), Some(b)) => (a, b),
            (None, _) => {
                unavailable.push(serde_json::json!({
                    "title": kpi.title,
                    "missing_member": baseline_member,
                }));
                continue;
            }
            (_, None) => {
                unavailable.push(serde_json::json!({
                    "title": kpi.title,
                    "missing_member": experiment_member,
                }));
                continue;
            }
        };

        let plot_id = format!(
            "kpi-{}-{}",
            kpi.role,
            crate::dashboard::service::slug(&kpi.title)
        );

        let group = match groups.iter_mut().find(|(r, _)| *r == kpi.role) {
            Some((_, g)) => g,
            None => {
                groups.push((
                    kpi.role.clone(),
                    Group::new(
                        crate::dashboard::service::capitalize(&kpi.role),
                        format!("kpi-{}", kpi.role),
                    ),
                ));
                &mut groups.last_mut().unwrap().1
            }
        };

        let opts = match kpi.metric_type.as_str() {
            "gauge" => PlotOpts::gauge(&kpi.title, &plot_id, Unit::Count),
            "histogram" => PlotOpts::histogram(
                &kpi.title,
                &plot_id,
                Unit::Count,
                kpi.subtype.as_deref().unwrap_or("percentiles"),
            ),
            _ => PlotOpts::counter(&kpi.title, &plot_id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
        let opts = match &kpi.percentiles {
            Some(p) => opts.with_percentiles(p.clone()),
            None => opts,
        };

        let sg = match kpi.subgroup.as_deref() {
            Some(name) => {
                if group.find_subgroup(name).is_none() {
                    let new_sg = group.subgroup(name);
                    if let Some(desc) = kpi.subgroup_description.as_deref() {
                        new_sg.describe(desc);
                    }
                    new_sg
                } else {
                    group.find_subgroup(name).unwrap()
                }
            }
            None => group.default_subgroup(),
        };

        let baseline_query = kpi.effective_query(&baseline_kpi.query);
        let experiment_query = kpi.effective_query(&experiment_kpi.query);
        if kpi.full_width {
            sg.plot_promql_full(opts, baseline_query.clone());
        } else {
            sg.plot_promql(opts, baseline_query.clone());
        }
        if experiment_query != baseline_query {
            if let Some(plot) = sg.plots_mut_last() {
                plot.promql_query_experiment = Some(experiment_query);
            }
        }
    }

    if !unavailable.is_empty() {
        view.metadata.insert(
            "bridge_unavailable".to_string(),
            serde_json::Value::Array(unavailable),
        );
    }

    for (_, group) in groups {
        view.group(group);
    }
    view
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p dashboard bridge_generate_records_unavailable_when_member_lookup_misses
cargo test -p dashboard
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/dashboard/bridge.rs
git commit -m "feat(dashboard): bridge::generate records unavailable KPIs"
```

---

### Task 7: Wire bridge into top-level `dashboard::generate`

**Files:**
- Modify: `crates/dashboard/src/dashboard/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `mod tests` block in `crates/dashboard/src/dashboard/mod.rs`:

```rust
#[test]
fn generate_emits_bridge_section_when_bridge_supplied() {
    use crate::service_extension::{BridgeExtension, BridgeKpi, Kpi, ServiceExtension};
    use std::collections::HashMap;

    let kpi = |role: &str, title: &str, query: &str| Kpi {
        role: role.to_string(),
        title: title.to_string(),
        description: None,
        query: query.to_string(),
        metric_type: "delta_counter".to_string(),
        subtype: None,
        unit_system: Some("rate".to_string()),
        percentiles: None,
        available: true,
        denominator: false,
        subgroup: None,
        subgroup_description: None,
        full_width: false,
    };
    let vllm = ServiceExtension {
        service_name: "vllm".to_string(),
        aliases: vec![],
        service_metadata: HashMap::new(),
        slo: None,
        kpis: vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
    };
    let sglang = ServiceExtension {
        service_name: "sglang".to_string(),
        aliases: vec![],
        service_metadata: HashMap::new(),
        slo: None,
        kpis: vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
    };
    let bridge = BridgeExtension {
        service_name: "inference-library".to_string(),
        bridge: true,
        members: vec!["vllm".to_string(), "sglang".to_string()],
        kpis: vec![BridgeKpi {
            role: "throughput".to_string(),
            title: "Generation Token Rate".to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
            member_titles: HashMap::new(),
        }],
    };

    let data = Tsdb::default();
    let result = generate(
        &data,
        None,
        &[("vllm", &vllm), ("sglang", &sglang)],
        Some(("inference-library", &bridge)),
        None,
    );

    // Bridge section present.
    assert!(result.contains_key("service/inference-library.json"));
    // Per-member sections absent.
    assert!(!result.contains_key("service/vllm.json"));
    assert!(!result.contains_key("service/sglang.json"));
}
```

- [ ] **Step 2: Run test — expect compile failure (signature mismatch)**

```bash
cargo test -p dashboard generate_emits_bridge_section_when_bridge_supplied 2>&1 | tail -15
```

Expected: signature mismatch on `generate`.

- [ ] **Step 3: Update `dashboard::generate` signature + body**

In `crates/dashboard/src/dashboard/mod.rs`:

```rust
use crate::service_extension::{BridgeExtension, ServiceExtension};

pub fn generate(
    data: &Tsdb,
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    bridge: Option<(&str, &BridgeExtension)>,
    _descriptions: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    // Build the section list. In bridge mode, the per-member sections
    // are replaced by a single bridge section.
    let mut all_sections: Vec<Section> = std::iter::once(Section {
        name: "Overview".to_string(),
        route: "/overview".to_string(),
    })
    .chain(SECTION_META.iter().map(|(name, route, _)| Section {
        name: (*name).to_string(),
        route: (*route).to_string(),
    }))
    .collect();

    if let Some((bridge_name, _)) = bridge {
        all_sections.insert(
            1,
            Section {
                name: bridge_name.to_string(),
                route: format!("/service/{bridge_name}"),
            },
        );
    } else {
        for (i, (source_name, _)) in service_exts.iter().enumerate() {
            all_sections.insert(
                1 + i,
                Section {
                    name: source_name.to_string(),
                    route: format!("/service/{source_name}"),
                },
            );
        }
    }

    let mut rendered = std::collections::HashMap::new();

    let throughput_query = service_exts
        .first()
        .and_then(|(_, e)| e.throughput_query())
        .map(str::to_string);
    {
        let mut view = overview::generate(data, all_sections.clone(), throughput_query.as_deref());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered.insert(
            "overview.json".to_string(),
            serde_json::to_string(&view).unwrap(),
        );
    }

    for (_, route, generator) in SECTION_META {
        let key = format!("{}.json", &route[1..]);
        let mut view = generator(data, all_sections.clone());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered.insert(key, serde_json::to_string(&view).unwrap());
    }

    if let Some((bridge_name, bridge_ext)) = bridge {
        if service_exts.len() == 2 {
            let (a_name, a_ext) = service_exts[0];
            let (b_name, b_ext) = service_exts[1];
            let view = bridge::generate(
                data,
                all_sections.clone(),
                bridge_ext,
                a_name,
                a_ext,
                b_name,
                b_ext,
            );
            let key = format!("service/{bridge_name}.json");
            rendered.insert(key, serde_json::to_string(&view).unwrap());
        }
    } else {
        for (source_name, ext) in service_exts {
            let view = service::generate(data, all_sections.clone(), ext);
            let key = format!("service/{source_name}.json");
            rendered.insert(key, serde_json::to_string(&view).unwrap());
        }
    }

    rendered
}
```

- [ ] **Step 4: Update existing callers to pass `None` for the new bridge arg**

Run:

```bash
grep -rn "dashboard::dashboard::generate\|dashboard::generate" src/ crates/ 2>&1 | grep -v "//\|target/"
```

For each call site, add `None,` for the new bridge argument. Expected sites:
- `src/viewer/mod.rs:281`
- `src/viewer/mod.rs:1417`
- `crates/viewer/src/lib.rs:85`
- `crates/viewer/src/lib.rs:297`

In each case the call looks like:

```rust
dashboard::dashboard::generate(&tsdb, filesize_opt, &service_refs, None);
```

becomes:

```rust
dashboard::dashboard::generate(&tsdb, filesize_opt, &service_refs, None, None);
```

(Two trailing `None`s now: bridge first, descriptions last.)

Also update the existing `generate_produces_expected_keys` test in the same `mod tests`:

```rust
let result = generate(&data, None, &[], None, None);
```

- [ ] **Step 5: Run test to verify everything compiles + new test passes**

```bash
cargo check --workspace
cargo test -p dashboard
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/dashboard/src/dashboard/mod.rs \
        src/viewer/mod.rs \
        crates/viewer/src/lib.rs
git commit -m "feat(dashboard): plumb bridge through top-level generate()"
```

---

### Task 8: Server viewer detects bridge in compare mode

**Files:**
- Modify: `src/viewer/mod.rs`

The server has two callers of `dashboard::dashboard::generate`. Both need to consult `find_bridge` when there are exactly two service extensions and pass the matching bridge, if any.

- [ ] **Step 1: Add a helper near the bottom of `src/viewer/mod.rs`**

```rust
/// Look up a bridge whose members exactly match the two service
/// extensions, in any order. Returns `None` for 0/1/3+ extensions or
/// when the registry has no matching bridge.
fn lookup_bridge<'a>(
    registry: &'a TemplateRegistry,
    service_refs: &[(&str, &ServiceExtension)],
) -> Option<(&'a str, &'a BridgeExtension)> {
    if service_refs.len() != 2 {
        return None;
    }
    let bridge = registry.find_bridge(service_refs[0].0, service_refs[1].0)?;
    Some((bridge.service_name.as_str(), bridge))
}
```

Add the corresponding `use` at the top of the file:

```rust
use dashboard::service_extension::{BridgeExtension, ServiceExtension, TemplateRegistry};
```

(Adjust to match the existing `dashboard::*` import style — `grep -n "use dashboard" src/viewer/mod.rs` first; some imports may already be present.)

- [ ] **Step 2: Update the file-mode dashboard generation site (around line 281)**

Find the block:

```rust
let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
let rendered = dashboard::dashboard::generate(&data, filesize, &service_refs, None);
```

Replace with:

```rust
let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
let bridge = lookup_bridge(&registry, &service_refs);
let rendered = dashboard::dashboard::generate(&data, filesize, &service_refs, bridge, None);
```

- [ ] **Step 3: Update the experiment-attach dashboard generation site (around line 1417)**

Same pattern. Find the existing call and add `bridge` as the new arg. Reuse the `state.templates` registry handle that's already in scope.

```rust
let service_refs: Vec<_> = service_exts.iter().map(|(s, e)| (s.as_str(), e)).collect();
let bridge = lookup_bridge(&state.templates, &service_refs);
let rendered = dashboard::dashboard::generate(&data, filesize, &service_refs, bridge, None);
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo check --workspace
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/viewer/mod.rs
git commit -m "feat(viewer): server consults find_bridge in compare mode"
```

---

### Task 9: WASM viewer detects bridge in `init_templates`

**Files:**
- Modify: `crates/viewer/src/lib.rs`

- [ ] **Step 1: Find the existing dashboard regeneration in `init_templates`**

```bash
grep -n "dashboard::dashboard::generate\|dashboard::generate" crates/viewer/src/lib.rs
```

Two call sites: the `Viewer::new` constructor (line ~85) and `Viewer::init_templates` (line ~297). The constructor has no service extensions, so it stays at `None, None`. The `init_templates` site needs the bridge detection.

- [ ] **Step 2: Update `init_templates`**

Find the block in `Viewer::init_templates`:

```rust
let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
    .iter()
    .map(|(name, ext)| (name.as_str(), ext))
    .collect();

self.dashboard_sections =
    dashboard::dashboard::generate(&*self.engine.tsdb(), None, &service_refs, None);
```

Replace the call with:

```rust
let service_refs: Vec<(&str, &dashboard::ServiceExtension)> = service_exts
    .iter()
    .map(|(name, ext)| (name.as_str(), ext))
    .collect();

let bridge = if service_refs.len() == 2 {
    registry
        .find_bridge(service_refs[0].0, service_refs[1].0)
        .map(|b| (b.service_name.as_str(), b))
} else {
    None
};

self.dashboard_sections = dashboard::dashboard::generate(
    &*self.engine.tsdb(),
    None,
    &service_refs,
    bridge,
    None,
);
```

`registry` is the local `TemplateRegistry` already constructed earlier in `init_templates`.

- [ ] **Step 3: Update the `Viewer::new` constructor's call site**

It already passes an empty service_refs list. Just add the new arg:

```rust
let dashboard_sections =
    dashboard::dashboard::generate(&tsdb, None, &[], None, None);
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo check --workspace
./crates/viewer/build.sh 2>&1 | tail -3
```

Expected: clean compile, WASM rebuilds.

- [ ] **Step 5: Commit**

```bash
git add crates/viewer/src/lib.rs site/viewer/pkg/
git commit -m "feat(wasm-viewer): consult find_bridge in init_templates"
```

(If the wasm artifacts under `site/viewer/pkg/` show as modified, include them. If only metadata changed and bytes match, skip them.)

---

### Task 10: `CompareChartWrapper` honors `promql_query_experiment`

**Files:**
- Modify: `src/viewer/assets/lib/viewer_core.js`

- [ ] **Step 1: Locate the experiment query construction**

```bash
grep -n "buildEffectiveQuery\|crossCapture: true" src/viewer/assets/lib/viewer_core.js
```

The single relevant line is in `fetchExperimentResult` (around line 207):

```js
const query = buildEffectiveQuery(spec, {
    sectionRoute,
    crossCapture: true,
});
```

- [ ] **Step 2: Wrap the spec to swap in the experiment-side query when present**

Replace those lines with:

```js
// Bridge templates supply a per-side experiment query via
// spec.promql_query_experiment. When present, route it through
// buildEffectiveQuery instead of spec.promql_query so the same
// histogram/counter rewrites and step substitutions apply.
const baseQuery = spec.promql_query_experiment || spec.promql_query;
const query = buildEffectiveQuery(
    { ...spec, promql_query: baseQuery },
    { sectionRoute, crossCapture: true },
);
```

- [ ] **Step 3: Syntax check**

```bash
node --check src/viewer/assets/lib/viewer_core.js && node --test tests/*.mjs 2>&1 | tail -3
```

Expected: OK; tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/viewer/assets/lib/viewer_core.js
git commit -m "feat(viewer): CompareChartWrapper uses promql_query_experiment when set"
```

---

### Task 11: Section header surfaces `bridge_members`; unavailable list shows `bridge_unavailable`

**Files:**
- Modify: `src/viewer/assets/lib/service.js`

- [ ] **Step 1: Locate the existing service section render**

```bash
grep -n "service_name\|unavailable_kpis\|bridge_members\|bridge_unavailable" src/viewer/assets/lib/service.js
```

The function is `renderServiceSection` (around line 14). It reads `meta.service_name` and `meta.unavailable_kpis`.

- [ ] **Step 2: Update `renderServiceSection` to surface bridge metadata**

Replace the title and unavailable-list portions:

```js
const renderServiceSection = (attrs, Group, sectionRoute, sectionName, interval, instanceOpts = {}) => {
    const meta = attrs.metadata || {};
    const serviceName = meta.service_name || 'Service';
    const serviceMeta = meta.service_metadata || {};
    const unavailable = meta.unavailable_kpis || [];
    const bridgeMembers = Array.isArray(meta.bridge_members) ? meta.bridge_members : null;
    const bridgeUnavailable = Array.isArray(meta.bridge_unavailable) ? meta.bridge_unavailable : [];
    const { instances = [], selectedInstance = null, onInstanceChange } = instanceOpts;
    const hasMultiInstance = instances.length > 1;

    const headerTitle = bridgeMembers
        ? `${serviceName} — ${bridgeMembers.join(' vs ')}`
        : serviceName;

    return m('div#section-content', [
        m('h1', headerTitle),
        // ... (instance selector + service metadata table unchanged)
        hasMultiInstance && m('div.instance-selector', [
            m('select.instance-select', {
                value: selectedInstance || '__all__',
                onchange: (e) => {
                    const val = e.target.value === '__all__' ? null : e.target.value;
                    if (onInstanceChange) onInstanceChange(val);
                },
            }, [
                m('option', { value: '__all__' }, 'All Instances'),
                ...instances.map(inst => {
                    const label = inst.node
                        ? `Instance ${inst.id} (${inst.node})`
                        : `Instance ${inst.id}`;
                    return m('option', { value: inst.id }, label);
                }),
            ]),
        ]),
        Object.keys(serviceMeta).length > 0
            ? m('table.sysinfo-table', [
                m('tbody', Object.entries(serviceMeta).map(([k, v]) =>
                    m('tr', [m('td.sysinfo-key', k), m('td', v)])
                )),
            ])
            : null,
        m('div#groups',
            (attrs.groups || []).map((group) =>
                m(Group, { ...group, sectionRoute, sectionName, interval })
            )
        ),
        unavailable.length > 0 && m('div.section-notes', [
            m('h3', 'Unavailable KPIs'),
            m('p', 'The following KPIs have no matching data in this recording:'),
            m('ul', unavailable.map((kpi) =>
                m('li', [m('strong', kpi.title), ` (${kpi.role})`])
            )),
        ]),
        bridgeUnavailable.length > 0 && m('div.section-notes', [
            m('h3', 'Bridge Skipped'),
            m('p', 'The following bridge KPIs were skipped because one member did not have a matching chart:'),
            m('ul', bridgeUnavailable.map((entry) =>
                m('li', [
                    m('strong', entry.title),
                    ` — missing in `,
                    m('code', entry.missing_member),
                ])
            )),
        ]),
    ]);
};
```

- [ ] **Step 3: Syntax check**

```bash
node --check src/viewer/assets/lib/service.js && node --test tests/*.mjs 2>&1 | tail -3
```

Expected: OK; tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/viewer/assets/lib/service.js
git commit -m "feat(viewer): surface bridge_members + bridge_unavailable in service section"
```

---

### Task 12: Add `inference-library.json` and register it for the static site

**Files:**
- Create: `config/templates/inference-library.json`
- Modify: `site/viewer/lib/script.js` (the static-site template list)

- [ ] **Step 1: Sketch the bridge content by reading the member templates**

```bash
grep '"title"' config/templates/vllm.json
grep '"title"' config/templates/sglang.json
```

- [ ] **Step 2: Write `config/templates/inference-library.json`**

Create the file with content (titles already match between vLLM and SGLang based on existing files; add `member_titles` only when a future divergence appears):

```json
{
  "service_name": "inference-library",
  "bridge": true,
  "members": ["vllm", "sglang"],
  "kpis": [
    {
      "role": "load",
      "title": "Prompt Token Rate",
      "type": "delta_counter",
      "unit_system": "rate"
    },
    {
      "role": "load",
      "title": "Requests Running",
      "type": "gauge",
      "unit_system": "count"
    },
    {
      "role": "load",
      "title": "Requests Waiting",
      "type": "gauge",
      "unit_system": "count"
    },
    {
      "role": "throughput",
      "title": "Generation Token Rate",
      "type": "delta_counter",
      "unit_system": "rate",
      "denominator": true
    },
    {
      "role": "latency",
      "title": "Time to First Token (TTFT)",
      "type": "histogram",
      "subtype": "percentiles",
      "percentiles": [0.5, 0.95],
      "unit_system": "time"
    }
  ]
}
```

- [ ] **Step 3: Register the bridge file in the static site's template list**

```bash
grep -n "templateNames" site/viewer/lib/script.js
```

The line declares which JSON files the WASM viewer fetches at boot. Update it:

```js
const templateNames = ['cachecannon', 'inference-library', 'llm-perf', 'sglang', 'valkey', 'vllm'];
```

- [ ] **Step 4: Smoke check**

```bash
cargo check -p rezolus
node --check site/viewer/lib/script.js
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add config/templates/inference-library.json site/viewer/lib/script.js
git commit -m "feat(viewer): inference-library bridge template (vllm + sglang)"
```

---

### Task 13: End-to-end smoke + WASM rebuild + version bump

**Files:**
- Modify: `Cargo.toml` (version bump)
- Build: WASM artifacts in `site/viewer/pkg/`

- [ ] **Step 1: Bump the version**

```bash
grep "^version" Cargo.toml
```

If current is `5.11.1-alpha.10`, change to `5.11.1-alpha.11` (or the next-higher revision):

```bash
git show main:Cargo.toml | grep '^version'
```

Use that value plus one. Edit `Cargo.toml`:

```
version = "5.11.1-alpha.11"
```

Then refresh the lockfile:

```bash
cargo check -p rezolus
```

- [ ] **Step 2: Rebuild WASM**

```bash
./crates/viewer/build.sh 2>&1 | tail -3
```

Expected: `Done in …`.

- [ ] **Step 3: Run all checks**

```bash
cargo test -p dashboard
cargo check --workspace
cargo fmt --check
node --test tests/*.mjs 2>&1 | tail -3
```

Expected: all green.

- [ ] **Step 4: Manual smoke test (server viewer, two captures of different services)**

```bash
cargo build --features developer-mode --bin rezolus
./target/debug/rezolus view site/viewer/data/vllm_gemma3.parquet site/viewer/data/sglang_gemma3.parquet --listen 127.0.0.1:4499
```

Open `http://127.0.0.1:4499`. Confirm:

- Sidebar shows a single `inference-library` entry (NOT separate `vllm` and `sglang` entries).
- `/service/inference-library` renders with header `inference-library — vllm vs sglang`.
- Each plot shows two series (baseline blue + experiment green) with non-empty data on both.
- Reset to `/cpu` and other rezolus sections — they still render normally.

Stop the server after verification:

```bash
pkill -f "target/debug/rezolus view"
```

- [ ] **Step 5: Manual smoke test (static site, WASM viewer)**

```bash
cd site && python3 -m http.server 8000
```

Open `http://localhost:8000/viewer/?capture=vllm_gemma3.parquet&capture=sglang_gemma3.parquet`. Same checks as Step 4.

Stop with Ctrl+C. Return to repo root.

- [ ] **Step 6: Commit version bump and WASM artifacts (if changed)**

```bash
git add Cargo.toml Cargo.lock site/viewer/pkg/
git commit -m "chore: bump version + rebuild WASM for inference-library bridge"
```

If WASM artifacts came back byte-identical except for `rezolus_webview_bg.wasm` (pre-existing dead artifact), revert that one:

```bash
git checkout HEAD -- site/viewer/pkg/rezolus_webview_bg.wasm 2>/dev/null || true
```

before committing.

---

## Execution notes

- Tasks 1–7 are pure Rust with unit tests; each is independently verifiable via `cargo test -p dashboard`.
- Tasks 8–9 add no new tests; they're integration glue. Verify by `cargo check --workspace` and the manual smoke test in Task 13.
- Tasks 10–11 are JS; verify by `node --check` and the manual smoke test.
- Task 12 is data only.
- Task 13 is the only task that requires running the binary; everything before it can be committed without launching the viewer.

## Out of scope (do not implement here)

- N-way (3+ member) bridges. The `members.len() != 2` validation in Task 3 is the gate.
- A UX for showing member-specific (non-bridged) KPIs alongside the bridge view.
- Unit conversion between members (e.g. seconds vs milliseconds).
- Bridge-aware cgroup or single-chart routes. Cgroups doesn't apply (cgroup is rezolus-side, not service-side); single-chart inherits the bridge's plots verbatim through the existing route.
