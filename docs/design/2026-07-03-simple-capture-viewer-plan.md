# Simple-Capture Viewer Support — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `rezolus view` useful on any legal metrics parquet that is neither a Rezolus-agent recording nor covered by a service-extension template, via per-source detection and a generic `source: <name>` metric-browser section.

**Architecture:** The viewer classifies each source (`Rezolus`/`Service`/`Simple`) and emits nav entries; the `dashboard` crate stays a passive renderer. A new `/api/v1/metrics?source=<name>` endpoint returns a metric catalog assembled from the existing TSDB accessors. The frontend special-cases the `/source/<name>` route to render an interactive `MetricBrowser` (searchable table → type-appropriate charts) reusing the existing query + chart pipeline.

**Tech Stack:** Rust (axum, serde, metriken-query), Mithril.js frontend, `node --test` for pure-JS tests, `tests/viewer_smoke.sh` for e2e.

## Global Constraints

- Detection lives in the **viewer** (`src/viewer/`); the `dashboard` crate never inspects metadata to classify — it renders what it is given.
- Rezolus self-sampler fingerprint anchors on the **cross-platform** rusage self-metrics `rezolus_cpu_usage`, `rezolus_memory_usage_resident_set_size`, `rezolus_rusage` — **never** `rezolus_bpf_run_count`/`rezolus_bpf_run_time` (Linux/eBPF-only; absent on macOS).
- No changes to `metriken-query`; use its existing accessors.
- TDD: failing test first, minimal impl, green, commit. Follow existing file patterns.
- Frontend assets are shared between the server viewer and the WASM static site via symlinks; anything added under `src/viewer/assets/lib/` must work in both (calls go through `viewer_api.js`'s `backendRequest`).

---

## File Structure

**Create:**
- `src/viewer/source_kind.rs` — `SourceKind`, `detect_source_kind`, source-name resolution. (Detection lives in the viewer.)
- `src/viewer/metric_catalog.rs` — `MetricInfo`/`MetricsResponse` DTOs + `assemble_catalog`.
- `src/viewer/assets/lib/features/metric_browser.js` — the interactive table+charts component.
- `tests/metric_type_defaults.test.mjs` — pure-JS test for `buildDefaultQuery`.

**Modify:**
- `src/viewer/mod.rs` — register the two new modules.
- `src/viewer/metadata.rs:374` (`regenerate_dashboards`) — classify sources, pass the decided section list down.
- `crates/dashboard/src/dashboard/mod.rs:61` (`build_dashboard_context`) — accept a passive list of `(kind, name)` and gate built-ins / add `source:` entries.
- `src/viewer/routes.rs:50` — register `/api/v1/metrics`; add the handler.
- `src/viewer/assets/lib/viewer_api.js:135` — add `getMetrics(source)`.
- `src/viewer/assets/lib/charts/metric_types.js` — add `buildDefaultQuery`.
- `src/viewer/assets/lib/app.js:484` (`SectionContent`) — special-case `/source/` route.
- `crates/viewer/src/lib.rs:201` — add a WASM `metrics(source)` method mirroring `info()`.
- `tests/viewer_smoke.sh` — add a simple-capture fixture + assertions.

---

## Phase 1 — Detection + section wiring

### Task 1: `SourceKind` + `detect_source_kind` + name resolution

**Files:**
- Create: `src/viewer/source_kind.rs`
- Modify: `src/viewer/mod.rs` (add `mod source_kind;`)
- Test: inline `#[cfg(test)]` in `src/viewer/source_kind.rs`

**Interfaces:**
- Produces:
  - `pub enum SourceKind { Rezolus, Service, Simple }`
  - `pub fn detect_source_kind(source: &str, has_sampler_status: bool, has_systeminfo: bool, has_template: bool, metric_names: &[String]) -> SourceKind`
  - `pub fn resolve_source_name(kind: SourceKind, source: &str, node: Option<&str>, filename_stem: Option<&str>) -> String`
  - `pub const REZOLUS_SELF_ANCHORS: &[&str]` — the cross-platform anchor metric names.

- [ ] **Step 1: Write the failing tests**

```rust
// at bottom of src/viewer/source_kind.rs
#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn metadata_marker_source_rezolus() {
        assert_eq!(detect_source_kind("rezolus", false, false, false, &[]), SourceKind::Rezolus);
    }

    #[test]
    fn metadata_marker_sampler_status() {
        assert_eq!(detect_source_kind("anything", true, false, false, &[]), SourceKind::Rezolus);
    }

    #[test]
    fn template_makes_service() {
        assert_eq!(detect_source_kind("llm-perf", false, false, true, &[]), SourceKind::Service);
    }

    #[test]
    fn self_sampler_fingerprint_linux() {
        let m = names(&["rezolus_cpu_usage", "rezolus_rusage", "cpu_usage"]);
        assert_eq!(detect_source_kind("", false, false, false, &m), SourceKind::Rezolus);
    }

    #[test]
    fn self_sampler_fingerprint_macos_no_bpf() {
        // macOS recording: rusage self-metrics present, NO rezolus_bpf_* at all.
        let m = names(&["rezolus_cpu_usage", "rezolus_memory_usage_resident_set_size"]);
        assert_eq!(detect_source_kind("", false, false, false, &m), SourceKind::Rezolus);
    }

    #[test]
    fn foreign_metrics_are_simple() {
        let m = names(&["http_requests_total", "queue_depth"]);
        assert_eq!(detect_source_kind("", false, false, false, &m), SourceKind::Simple);
    }

    #[test]
    fn name_resolution() {
        assert_eq!(resolve_source_name(SourceKind::Rezolus, "rezolus", Some("node7"), None), "node7");
        assert_eq!(resolve_source_name(SourceKind::Rezolus, "rezolus", None, Some("x")), "rezolus");
        assert_eq!(resolve_source_name(SourceKind::Simple, "svc", None, Some("cap")), "svc");
        assert_eq!(resolve_source_name(SourceKind::Simple, "", None, Some("cap")), "cap");
        assert_eq!(resolve_source_name(SourceKind::Simple, "", None, None), "metrics");
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p rezolus --lib viewer::source_kind 2>&1 | tail -20`
Expected: FAIL — `cannot find function detect_source_kind` (module not created yet).

- [ ] **Step 3: Write the implementation (top of the same file)**

```rust
//! Per-source classification for the viewer. Detection lives here (never in the
//! dashboard crate). Layered: metadata markers first, then a cross-platform
//! self-sampler fingerprint, else Simple.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Rezolus,
    Service,
    Simple,
}

/// Cross-platform Rezolus self-telemetry (rezolus/rusage sampler). Present on
/// both Linux and macOS. Deliberately excludes rezolus_bpf_* (Linux/eBPF-only).
pub const REZOLUS_SELF_ANCHORS: &[&str] = &[
    "rezolus_cpu_usage",
    "rezolus_memory_usage_resident_set_size",
    "rezolus_rusage",
];

pub fn detect_source_kind(
    source: &str,
    has_sampler_status: bool,
    has_systeminfo: bool,
    has_template: bool,
    metric_names: &[String],
) -> SourceKind {
    // Tier 1: metadata markers.
    if source == "rezolus" || has_sampler_status || has_systeminfo {
        return SourceKind::Rezolus;
    }
    if has_template {
        return SourceKind::Service;
    }
    // Tier 2: cross-platform self-sampler fingerprint.
    if metric_names.iter().any(|n| REZOLUS_SELF_ANCHORS.contains(&n.as_str())) {
        return SourceKind::Rezolus;
    }
    SourceKind::Simple
}

/// Resolve the display/section name for a source. Rezolus: node else "rezolus".
/// Simple: explicit source -> filename stem -> "metrics".
pub fn resolve_source_name(
    kind: SourceKind,
    source: &str,
    node: Option<&str>,
    filename_stem: Option<&str>,
) -> String {
    match kind {
        SourceKind::Rezolus => node.filter(|s| !s.is_empty()).unwrap_or("rezolus").to_string(),
        SourceKind::Service => source.to_string(),
        SourceKind::Simple => {
            if !source.is_empty() {
                source.to_string()
            } else if let Some(stem) = filename_stem.filter(|s| !s.is_empty()) {
                stem.to_string()
            } else {
                "metrics".to_string()
            }
        }
    }
}
```

Add to `src/viewer/mod.rs` near the other `mod` lines:

```rust
mod source_kind;
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test -p rezolus --lib viewer::source_kind 2>&1 | tail -20`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add src/viewer/source_kind.rs src/viewer/mod.rs
git commit -m "feat(viewer): per-source SourceKind detection with cross-platform fingerprint"
```

---

### Task 2: Wire classification into dashboard nav (gate built-ins, add `source:` entries)

**Files:**
- Modify: `src/viewer/metadata.rs:374` (`regenerate_dashboards`)
- Modify: `crates/dashboard/src/dashboard/mod.rs:61` (`build_dashboard_context`)
- Test: inline `#[cfg(test)]` in `crates/dashboard/src/dashboard/mod.rs`

**Interfaces:**
- Consumes: `SourceKind` (Task 1) — but the dashboard crate must NOT depend on the viewer. Pass a plain descriptor instead.
- Produces:
  - `pub struct SourceEntry { pub name: String, pub is_rezolus: bool }` in the dashboard crate.
  - Extended `build_dashboard_context(filesize, service_exts, category, sources: &[SourceEntry]) -> DashboardContext`, where `sources` lists every non-service source with whether it is Rezolus. Built-in `SECTION_META` sections + Overview are included only if `sources.iter().any(|s| s.is_rezolus)`. Each non-Rezolus `SourceEntry` yields a `Section { name: format!("source: {name}"), route: format!("/source/{name}") }`.

- [ ] **Step 1: Write the failing test**

```rust
// in crates/dashboard/src/dashboard/mod.rs #[cfg(test)] mod tests
#[test]
fn simple_only_file_has_no_builtins_and_one_source_section() {
    let sources = vec![SourceEntry { name: "myapp".into(), is_rezolus: false }];
    let ctx = build_dashboard_context(None, &[], None, &sources);
    let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
    assert!(routes.contains(&"/source/myapp"));
    assert!(!routes.contains(&"/overview"));
    assert!(!routes.contains(&"/cpu"));
}

#[test]
fn mixed_file_shows_builtins_and_source_section() {
    let sources = vec![
        SourceEntry { name: "rezolus".into(), is_rezolus: true },
        SourceEntry { name: "myapp".into(), is_rezolus: false },
    ];
    let ctx = build_dashboard_context(None, &[], None, &sources);
    let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
    assert!(routes.contains(&"/overview"));
    assert!(routes.contains(&"/cpu"));
    assert!(routes.contains(&"/source/myapp"));
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test -p dashboard build_dashboard_context 2>&1 | tail -20`
Expected: FAIL — `SourceEntry` undefined / arity mismatch on `build_dashboard_context`.

- [ ] **Step 3: Implement**

In `crates/dashboard/src/dashboard/mod.rs`, add the descriptor and extend the builder. Add near `DashboardContext`:

```rust
/// Passive descriptor handed down by the viewer. The dashboard crate does not
/// classify — it renders what it is told.
pub struct SourceEntry {
    pub name: String,
    pub is_rezolus: bool,
}
```

Change `build_dashboard_context` signature and section assembly (around lines 61 and 88-117). Replace the unconditional Overview+SECTION_META chain with a gate, and append `source:` sections:

```rust
pub fn build_dashboard_context(
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
    sources: &[SourceEntry],
) -> DashboardContext {
    // ... existing dedup of service_exts (unchanged) ...

    let has_rezolus = sources.iter().any(|s| s.is_rezolus);

    let mut sections: Vec<Section> = Vec::new();
    if has_rezolus {
        sections.push(Section { name: "Overview".into(), route: "/overview".into() });
        sections.extend(SECTION_META.iter().map(|(name, route, _)| Section {
            name: (*name).to_string(),
            route: (*route).to_string(),
        }));
    }

    // ... existing service/category insertion (unchanged) ...

    // Append a section per non-Rezolus source (the Simple sources).
    for s in sources.iter().filter(|s| !s.is_rezolus) {
        sections.push(Section {
            name: format!("source: {}", s.name),
            route: format!("/source/{}", s.name),
        });
    }

    // ... existing throughput_query / owned_* / return (unchanged) ...
}
```

In `src/viewer/metadata.rs` `regenerate_dashboards`, build the `Vec<SourceEntry>` from the parsed file metadata before calling the builder. After the existing `service_refs`/`category` setup and before `build_dashboard_context` (line 434):

```rust
// Classify every source present in the recording.
let sources = classify_sources(state); // helper below
let context = dashboard::dashboard::build_dashboard_context(
    filesize, &service_refs, category, &sources,
);
```

Add a private helper in `metadata.rs` that reads the parsed file metadata (`source`, `per_source_metadata` for `sampler_status`/`node`, presence of `systeminfo`, whether a template matched) plus the baseline data's `counter_names()/gauge_names()/histogram_names()` and calls `detect_source_kind` + `resolve_source_name` per source, returning `Vec<dashboard::dashboard::SourceEntry>`. Service sources are represented via the existing `service_exts` path and excluded from `sources`. Single-source foreign file → one `SourceEntry { name: resolved, is_rezolus: false }`.

- [ ] **Step 4: Run tests, verify green**

Run: `cargo test -p dashboard build_dashboard_context 2>&1 | tail -20 && cargo build -p rezolus 2>&1 | tail -5`
Expected: dashboard tests PASS; `rezolus` builds (all `build_dashboard_context` call sites updated with the new arg).

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/dashboard/mod.rs src/viewer/metadata.rs
git commit -m "feat(viewer): gate built-in sections on Rezolus source; add source: nav entries"
```

---

## Phase 2 — Metric catalog + endpoint

### Task 3: Metric catalog assembler + DTO

**Files:**
- Create: `src/viewer/metric_catalog.rs`
- Modify: `src/viewer/mod.rs` (`mod metric_catalog;`)
- Test: inline `#[cfg(test)]` (unit-test the assembler against a small in-memory `MetricsSource` fake, or against a fixture parquet loaded via the existing reader).

**Interfaces:**
- Produces:
  - `#[derive(Serialize)] pub struct MetricInfo { pub name: String, pub metric_type: String, pub series_count: usize, pub label_keys: Vec<String>, pub description: Option<String> }`
  - `#[derive(Serialize)] pub struct MetricsResponse { pub source: String, pub metrics: Vec<MetricInfo> }`
  - `pub fn assemble_catalog(data: &dyn MetricsSource, descriptions: &serde_json::Map<String, serde_json::Value>, source_filter: Option<&str>) -> Vec<MetricInfo>`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Reuse the smallest existing MetricsSource fixture in the repo; if a fake
    // is impractical, load a checked-in fixture parquet via the viewer reader.
    // Assert: each metric name appears once with correct type; series_count ==
    // number of label sets; label_keys is the sorted union of label keys;
    // description is populated from the descriptions map when present.
    #[test]
    fn counter_gauge_histogram_are_typed_and_counted() {
        // ... construct fixture data with 1 counter (2 series), 1 gauge (1),
        //     1 histogram (3), and a descriptions entry for the counter ...
        // let cat = assemble_catalog(&data, &descriptions, None);
        // assert_eq!(find(&cat, "the_counter").metric_type, "counter");
        // assert_eq!(find(&cat, "the_counter").series_count, 2);
        // assert_eq!(find(&cat, "the_counter").description.as_deref(), Some("help"));
    }
}
```

> Implementer note: pick the fixture mechanism that matches how other viewer unit tests obtain a `MetricsSource` (grep `tests` under `src/viewer` and `crates/dashboard` for the existing pattern) and fill the test body with concrete asserts before implementing.

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test -p rezolus --lib viewer::metric_catalog 2>&1 | tail -20`
Expected: FAIL — `assemble_catalog` not found.

- [ ] **Step 3: Implement**

```rust
use metriken_query::MetricsSource;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub struct MetricInfo {
    pub name: String,
    pub metric_type: String,
    pub series_count: usize,
    pub label_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct MetricsResponse {
    pub source: String,
    pub metrics: Vec<MetricInfo>,
}

fn info_for(
    name: &str,
    metric_type: &str,
    labels: Vec<std::collections::BTreeMap<String, String>>,
    descriptions: &serde_json::Map<String, serde_json::Value>,
) -> MetricInfo {
    let keys: BTreeSet<String> = labels.iter().flat_map(|m| m.keys().cloned()).collect();
    MetricInfo {
        name: name.to_string(),
        metric_type: metric_type.to_string(),
        series_count: labels.len(),
        label_keys: keys.into_iter().collect(),
        description: descriptions.get(name).and_then(|v| v.as_str()).map(str::to_string),
    }
}

/// Assemble the metric catalog. `source_filter` is applied when the file is
/// combined (per-column source); for a single-source file it is ignored (every
/// metric belongs to the one source). See Task 5 for combined-file filtering.
pub fn assemble_catalog(
    data: &dyn MetricsSource,
    descriptions: &serde_json::Map<String, serde_json::Value>,
    _source_filter: Option<&str>,
) -> Vec<MetricInfo> {
    let mut out = Vec::new();
    for name in data.counter_names() {
        let labels = data.counter_labels(&name).unwrap_or_default();
        out.push(info_for(&name, "counter", labels, descriptions));
    }
    for name in data.gauge_names() {
        let labels = data.gauge_labels(&name).unwrap_or_default();
        out.push(info_for(&name, "gauge", labels, descriptions));
    }
    for name in data.histogram_names() {
        let labels = data.histogram_labels(&name).unwrap_or_default();
        out.push(info_for(&name, "histogram", labels, descriptions));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
```

Add `mod metric_catalog;` to `src/viewer/mod.rs`.

- [ ] **Step 4: Run test, verify green**

Run: `cargo test -p rezolus --lib viewer::metric_catalog 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/viewer/metric_catalog.rs src/viewer/mod.rs
git commit -m "feat(viewer): metric catalog assembler + MetricInfo DTO"
```

---

### Task 4: `/api/v1/metrics` endpoint

**Files:**
- Modify: `src/viewer/routes.rs` (register route at ~line 50; add handler near `systeminfo_handler` at ~line 213)
- Test: extend `tests/viewer_smoke.sh` assertion is done in Task 9; here add a focused Rust handler smoke via the existing route test harness if one exists, else rely on Task 9.

**Interfaces:**
- Consumes: `assemble_catalog` (Task 3), `MetricsResponse`, `AppState::captures`, `CaptureParam`.
- Produces: `GET /api/v1/metrics?source=<name>&capture=baseline|experiment` → `MetricsResponse` JSON.

- [ ] **Step 1: Add a query extractor + handler**

```rust
// near CaptureParam usage in routes.rs
#[derive(serde::Deserialize)]
struct MetricsParam {
    #[serde(default)]
    capture: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

async fn metrics_handler(
    State(state): State<Arc<AppState>>,
    Query(p): Query<MetricsParam>,
) -> Response {
    let capture_id = crate::viewer::state::CaptureId::parse_opt(p.capture.as_deref());
    let Some(data) = state.captures.get(capture_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // descriptions come from the parsed file_metadata JSON (key "descriptions").
    let descriptions = state
        .captures
        .file_metadata(capture_id)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("descriptions").and_then(|d| d.as_object()).cloned())
        .unwrap_or_default();

    let metrics = crate::viewer::metric_catalog::assemble_catalog(
        data.as_ref(), &descriptions, p.source.as_deref(),
    );
    let body = crate::viewer::metric_catalog::MetricsResponse {
        source: p.source.unwrap_or_else(|| data.source()),
        metrics,
    };
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], serde_json::to_string(&body).unwrap())
        .into_response()
}
```

Register in the `api_routes` builder (routes.rs ~line 50):

```rust
.route("/metrics", get(metrics_handler))
```

- [ ] **Step 2: Build & manual smoke**

Run: `cargo build -p rezolus 2>&1 | tail -5`
Then, against any parquet: `target/debug/rezolus view <file.parquet> --listen 127.0.0.1:18600 &` and `curl -s 'http://127.0.0.1:18600/api/v1/metrics' | head -c 400`
Expected: JSON `{"source":"...","metrics":[{"name":...,"metric_type":...,"series_count":...}]}`.

- [ ] **Step 3: Commit**

```bash
git add src/viewer/routes.rs
git commit -m "feat(viewer): GET /api/v1/metrics catalog endpoint"
```

---

## Phase 3 — Frontend metric browser

### Task 5: `buildDefaultQuery` pure function + test

**Files:**
- Modify: `src/viewer/assets/lib/charts/metric_types.js`
- Create: `tests/metric_type_defaults.test.mjs`

**Interfaces:**
- Produces: `export function buildDefaultQuery(metricInfo)` where `metricInfo = { name, metric_type }` → PromQL string. counter → `rate(name[<window>])`; gauge → `name`; histogram → `histogram_quantiles([...], name)` (reuse `buildHistogramQuery(name, 'percentiles', DEFAULT_PERCENTILES)`).

- [ ] **Step 1: Write the failing test**

```javascript
// tests/metric_type_defaults.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import { buildDefaultQuery } from '../src/viewer/assets/lib/charts/metric_types.js';

test('counter → rate over default window', () => {
  assert.match(buildDefaultQuery({ name: 'http_requests_total', metric_type: 'counter' }),
    /^rate\(http_requests_total\[\d+[smh]\]\)$/);
});
test('gauge → raw', () => {
  assert.equal(buildDefaultQuery({ name: 'queue_depth', metric_type: 'gauge' }), 'queue_depth');
});
test('histogram → percentiles', () => {
  assert.match(buildDefaultQuery({ name: 'req_latency', metric_type: 'histogram' }),
    /^histogram_quantiles\(\[.*\], req_latency\)$/);
});
```

- [ ] **Step 2: Run, verify fail**

Run: `node --test tests/metric_type_defaults.test.mjs`
Expected: FAIL — `buildDefaultQuery` is not exported.

- [ ] **Step 3: Implement in `metric_types.js`**

```javascript
// Default rate window for counters. Match the viewer's existing default
// (grep DEFAULT_RATE_WINDOW / '[5m]' in data.js and reuse the same value).
export const DEFAULT_RATE_WINDOW = '5m';
export const DEFAULT_PERCENTILES = [0.5, 0.9, 0.99];

export function buildDefaultQuery(metricInfo) {
  const { name, metric_type } = metricInfo;
  if (metric_type === 'histogram') {
    return buildHistogramQuery(name, 'percentiles', DEFAULT_PERCENTILES);
  }
  if (metric_type === 'counter') {
    return `rate(${name}[${DEFAULT_RATE_WINDOW}])`;
  }
  return name; // gauge
}
```

- [ ] **Step 4: Run, verify green**

Run: `node --test tests/metric_type_defaults.test.mjs`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/viewer/assets/lib/charts/metric_types.js tests/metric_type_defaults.test.mjs
git commit -m "feat(viewer): buildDefaultQuery type→query mapping + test"
```

---

### Task 6: `ViewerApi.getMetrics`

**Files:**
- Modify: `src/viewer/assets/lib/viewer_api.js` (after `getSection`, ~line 135)

**Interfaces:**
- Produces: `async getMetrics(source, captureId = 'baseline')` → resolves to `MetricsResponse`. Uses `backendRequest` so it works in both server and WASM builds.

- [ ] **Step 1: Add the method**

```javascript
async getMetrics(source = null, captureId = 'baseline') {
  const params = new URLSearchParams();
  if (source) params.set('source', source);
  if (captureId) params.set('capture', captureId);
  const qs = params.toString();
  return backendRequest({ method: 'GET', url: `/api/v1/metrics${qs ? `?${qs}` : ''}` });
}
```

- [ ] **Step 2: Sanity build (bundle-free; just lint by loading in node)**

Run: `node -e "import('./src/viewer/assets/lib/viewer_api.js').then(()=>console.log('ok')).catch(e=>{console.error(e);process.exit(1)})"`
Expected: `ok` (module parses). If it imports Mithril at top level and fails in node, skip this and rely on the smoke test in Task 9.

- [ ] **Step 3: Commit**

```bash
git add src/viewer/assets/lib/viewer_api.js
git commit -m "feat(viewer): ViewerApi.getMetrics(source) client method"
```

---

### Task 7: `MetricBrowser` component + `/source/` route special-case + default landing

**Files:**
- Create: `src/viewer/assets/lib/features/metric_browser.js`
- Modify: `src/viewer/assets/lib/app.js` (`SectionContent`, ~line 484; and the default-route/redirect logic)

**Interfaces:**
- Consumes: `ViewerApi.getMetrics` (Task 6), `buildDefaultQuery` (Task 5), `executePromQLRangeQuery` (data.js:333), `applyResultToPlot` (data.js:220), the `Chart` component (charts/chart.js), `resolveStyle`/`compatibleStyles` (metric_types.js).
- Produces: `export function createMetricBrowser(sourceName)` returning a Mithril component; a `SectionContent` branch that renders it for routes under `/source/`.

- [ ] **Step 1: Write the component**

Follow the Query Explorer reference (`features/explorers.js:98,173`) for the query→chart render, and the table/selection is new. Concrete skeleton:

```javascript
// src/viewer/assets/lib/features/metric_browser.js
import m from 'mithril';
import { ViewerApi } from '../viewer_api.js';
import { buildDefaultQuery } from '../charts/metric_types.js';
import { executePromQLRangeQuery } from '../data.js';
import { applyResultToPlot } from '../data.js';
import { Chart } from '../charts/chart.js';

export function createMetricBrowser(sourceName) {
  return {
    oninit(vnode) {
      const st = vnode.state;
      st.filter = '';
      st.metrics = [];
      st.selected = new Map(); // name -> plot object
      ViewerApi.getMetrics(sourceName).then((resp) => {
        st.metrics = (resp && resp.metrics) || [];
        m.redraw();
      });
      st.toggle = async (info) => {
        if (st.selected.has(info.name)) { st.selected.delete(info.name); m.redraw(); return; }
        const plot = { opts: { id: info.name, title: info.name, type: info.metric_type } };
        st.selected.set(info.name, plot);
        m.redraw();
        const result = await executePromQLRangeQuery(buildDefaultQuery(info));
        applyResultToPlot(plot, result);
        m.redraw();
      };
    },
    view(vnode) {
      const st = vnode.state;
      const f = st.filter.toLowerCase();
      const rows = st.metrics.filter((x) => x.name.toLowerCase().includes(f));
      return m('div.metric-browser', [
        m('input.metric-search', {
          placeholder: 'search metrics…',
          oninput: (e) => { st.filter = e.target.value; },
        }),
        m('table.metric-table', [
          m('thead', m('tr', ['', 'name', 'type', 'series', 'labels', 'description']
            .map((h) => m('th', h)))),
          m('tbody', rows.map((info) => m('tr', {
            class: st.selected.has(info.name) ? 'selected' : '',
            onclick: () => st.toggle(info),
          }, [
            m('td', m('input[type=checkbox]', { checked: st.selected.has(info.name) })),
            m('td', info.name),
            m('td', info.metric_type),
            m('td', String(info.series_count)),
            m('td', (info.label_keys || []).join(', ')),
            m('td', info.description || ''),
          ]))),
        ]),
        m('div.metric-charts', [...st.selected.values()].map((plot) =>
          m(Chart, { spec: plot }))),
      ]);
    },
  };
}
```

> Implementer note: confirm the exact `Chart` prop name (`spec` vs `plot`) at `charts/chart.js` and match it; confirm `applyResultToPlot`/`executePromQLRangeQuery` are exported from `data.js` (add `export` if they are module-local — they are referenced internally today).

- [ ] **Step 2: Special-case the `/source/` route in `SectionContent`**

In `app.js` `SectionContent.view` (~line 484), alongside the existing Query Explorer (line ~520) and `/service/` (line ~596) branches, add before the default `Group` path:

```javascript
if (attrs.activeSection && attrs.activeSection.route &&
    attrs.activeSection.route.startsWith('/source/')) {
  const name = attrs.activeSection.route.slice('/source/'.length);
  return m(createMetricBrowser(name));
}
```

Import at top of `app.js`: `import { createMetricBrowser } from './features/metric_browser.js';`

- [ ] **Step 3: Default landing when there is no Overview**

Find the default-route redirect in `app.js` (grep `m.route.set` / `'/overview'` / default section). Change it to land on the first section from the loaded sections list when `/overview` is absent:

```javascript
// pseudo: on initial load
const sections = getCachedSections();
const hasOverview = sections.some((s) => s.route === '/overview');
const landing = hasOverview ? '/overview' : (sections[0] ? sections[0].route : '/overview');
m.route.set(landing);
```

> Implementer note: match the existing redirect call shape exactly; only change the target selection.

- [ ] **Step 4: Manual verification (real app)**

Run: build the WASM bundle if needed (`./crates/viewer/build.sh`) is NOT required for the server viewer since assets are served from disk in developer-mode. Start:
`cargo run -p rezolus --features developer-mode -- view <simple-capture>.parquet --listen 127.0.0.1:18601`
Open the URL: expect a `source: <name>` section as the landing page, a searchable table, and clicking a metric renders a type-appropriate chart. Then open a Rezolus parquet: expect the normal dashboards unchanged, with an added `source:` section only if a foreign source is present.

- [ ] **Step 5: Commit**

```bash
git add src/viewer/assets/lib/features/metric_browser.js src/viewer/assets/lib/app.js
git commit -m "feat(viewer): MetricBrowser section (table → type-appropriate charts)"
```

---

## Phase 4 — WASM parity + end-to-end

### Task 8: WASM `metrics(source)` method

**Files:**
- Modify: `crates/viewer/src/lib.rs` (mirror `info()` at ~line 201)

**Interfaces:**
- Produces: a `#[wasm_bindgen] pub fn metrics(&self, source: Option<String>) -> String` returning the same `MetricsResponse` JSON the server endpoint returns, so `ViewerApi.getMetrics` works against the WASM backend.

- [ ] **Step 1: Implement, reusing the assembler shape**

```rust
// crates/viewer/src/lib.rs — mirror info()
pub fn metrics(&self, source: Option<String>) -> String {
    // Reuse the same per-metric assembly used server-side. If the assembler
    // lives in the rezolus binary crate (not shared), replicate the small loop
    // here over self.reader.{counter,gauge,histogram}_{names,labels}() and the
    // descriptions map, producing the identical JSON shape.
    // ... build Vec<MetricInfo> and serialize MetricsResponse ...
}
```

> Implementer note: to avoid duplication, consider moving `MetricInfo`/`assemble_catalog` into a shared location both `crates/viewer` and the `rezolus` binary depend on (e.g. the `dashboard` crate is shared — but keep it a pure data helper, not classification, to respect the passive-crate rule). Decide during implementation; either way the JSON shape must be byte-compatible with Task 3.

- [ ] **Step 2: Build the WASM bundle**

Run: `./crates/viewer/build.sh 2>&1 | tail -10`
Expected: builds to `site/viewer/pkg/` without error.

- [ ] **Step 3: Commit**

```bash
git add crates/viewer/src/lib.rs
git commit -m "feat(wasm-viewer): metrics(source) catalog method for static site parity"
```

---

### Task 9: Smoke test + fixture

**Files:**
- Modify: `tests/viewer_smoke.sh`
- Create: a small non-Rezolus fixture parquet (checked in under the smoke test's fixture dir, or generated in-script).

**Interfaces:**
- Consumes: `/api/v1/metrics`, `/api/v1/sections`.

- [ ] **Step 1: Add a simple-capture fixture**

Generate a tiny non-Rezolus parquet. Preferred: record a trivial Prometheus endpoint, or craft one with `rezolus record` from a static metrics file. Document the exact command used to (re)generate it in a comment at the top of the fixture section.

- [ ] **Step 2: Add assertions to `tests/viewer_smoke.sh`**

Following the existing assertion helpers in the script, add a mode that boots `rezolus view <simple-fixture>.parquet` and asserts:

```bash
# /api/v1/metrics returns a non-empty catalog with typed entries
metrics_json="$(curl -s "$BASE/api/v1/metrics")"
echo "$metrics_json" | jq -e '.metrics | length > 0' >/dev/null
echo "$metrics_json" | jq -e '.metrics[0] | has("name") and has("metric_type") and has("series_count")' >/dev/null

# nav has a source: section and NO empty Rezolus built-ins
sections_json="$(curl -s "$BASE/api/v1/sections")"
echo "$sections_json" | jq -e '[.data.sections[].route] | any(startswith("/source/"))' >/dev/null
echo "$sections_json" | jq -e '[.data.sections[].route] | any(. == "/cpu") | not' >/dev/null
```

- [ ] **Step 3: Run the smoke test**

Run: `bash tests/viewer_smoke.sh 2>&1 | tail -30`
Expected: exit 0; the new simple-capture assertions pass alongside the existing modes.

- [ ] **Step 4: Commit**

```bash
git add tests/viewer_smoke.sh tests/fixtures/*
git commit -m "test(viewer): simple-capture smoke coverage (metrics catalog + nav)"
```

---

## Task 5b (deferred, combined-file source filtering)

**Only if combined-file per-source scoping is needed beyond the single-source MVP.**

`parquet combine` prefixes columns `{node-or-instance}::{metric}` and writes a per-column `source` in column metadata (`src/parquet_tools/combine.rs:830-858`). Before implementing the `source_filter` branch in `assemble_catalog`:

- [ ] **Step 1: Verify the read path.** Determine how `metriken-query` surfaces a combined file's per-metric source — is `source` reachable per metric (column metadata), or does the `node`/`instance` label distinguish them? Probe: `target/debug/rezolus parquet metadata -i <combined>.parquet --json | jq` and grep `metriken-query` for per-column metadata access.
- [ ] **Step 2: Implement the filter** in `assemble_catalog` using whatever mapping Step 1 confirms (e.g., filter metric names by the source's column-name prefix, or by an `instance`/`source` label), and add a unit test with a combined fixture.
- [ ] **Step 3: Commit.**

---

## Self-Review Notes

- **Spec coverage:** A (Task 1) · B (Task 2) · C (Tasks 5–7) · D (Tasks 3,4,6,8) · E (Tasks 1,3,5,9). Naming (Task 1). macOS fingerprint (Task 1 test). Passive dashboard crate (Task 2 uses a plain `SourceEntry`, no viewer types).
- **Known unverified seams** (flagged inline, not placeholders): the exact fixture mechanism for the Rust catalog test (Task 3), whether `executePromQLRangeQuery`/`applyResultToPlot` are exported vs module-local (Task 7), the `Chart` prop name (Task 7), the default-redirect call shape (Task 7), and combined-file source mapping (Task 5b). Each has a concrete verification step rather than a guess.
- **Right-sizing:** Phase boundaries are natural review checkpoints; Phase 1 alone fixes today's empty-Rezolus-sections UX and is independently shippable.
