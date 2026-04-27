# Inference Library — Service Bridge Template Design

**Status:** Draft (post-brainstorming)
**Owner:** thinkingfish
**Date:** 2026-04-27

## Problem

Service templates today (`config/templates/<name>.json`) define KPIs as `{role, title, query, type, ...}`. The viewer attaches one template per capture's source — vLLM recordings get `vllm.json`, SGLang recordings get `sglang.json`. In compare mode (`?capture=A&capture=B`), `CompareChartWrapper` runs the **same** PromQL query against both captures via the existing A/B overlay. That works when both captures are the same service. It breaks across heterogeneous services: querying `vllm_prompt_tokens_total` against an SGLang capture returns empty, so the green/experiment series renders as no-data.

Real-world A/B testing wants to compare semantically-equivalent KPIs across different inference engines — e.g. vLLM's "Time to First Token (TTFT)" against SGLang's "TTFT", or vLLM's `vllm_generation_tokens_total` against SGLang's `sglang_generation_tokens_total`. The chart titles and the user's intent are the same; the underlying metric names diverge.

## Goal

Introduce a **service bridge template** — a third kind of template file that ties two existing service templates together by enumerating cross-service KPI equivalences. When both captures of a compare-mode session match the bridge's two members, the viewer renders one synthesized `/service/<bridge>` section with unified plot specs. Each plot uses the existing A/B overlay rendering, but its baseline and experiment fetch each consult the corresponding member template's query, so both sides return real data.

The first concrete bridge ships as `inference-library.json`, joining `vllm.json` and `sglang.json`.

## Non-goals

- N-way (3+) bridging. Out of scope; the bridge file format permits only `members.length === 2`. Future extension can relax this.
- Mixing bridged KPIs with member-specific KPIs in the same view. v1 fully replaces both per-service sections with one bridge section. KPIs not enumerated in the bridge file disappear.
- A UI for authoring bridge files. They're hand-edited JSON in `config/templates/` like other service templates.
- Bridge-aware single-capture mode. With only one capture loaded, the bridge is inert and the regular per-service section renders.
- Per-capture role swap (which member is baseline vs experiment). The bridge handles whichever ordering the user attaches; if they swap captures, the dashboard regenerates with the new ordering.

## File format

A bridge template is structurally similar to `ServiceExtension` but flagged with `bridge: true` and missing per-KPI `query` strings (queries are looked up from the member templates):

```json
{
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
    },
    {
      "role": "latency",
      "title": "TTFT",
      "type": "histogram",
      "subtype": "percentiles",
      "unit_system": "time",
      "percentiles": [0.5, 0.95],
      "member_titles": {
        "vllm":   "Time to First Token (TTFT)",
        "sglang": "Time to First Token (TTFT)"
      }
    }
  ]
}
```

Conventions:

- `bridge: true` distinguishes bridges from regular `ServiceExtension`s. The shared loader (`TemplateRegistry::load` / `from_embedded`) sorts entries into `services` vs `bridges` based on this flag.
- `members` is required, must have exactly **two** entries, both of which must be loadable as regular service templates at runtime (validated at registry load time). 3+ rejects with an error.
- `member_titles` keys must be a subset of `members`. Missing entries default the lookup to the bridge KPI's own `title`. So users only fill in `member_titles` when titles diverge across members.
- Bridge KPIs carry the chart spec (`role`, `title`, `type`, `unit_system`, `subtype`, `percentiles`, `full_width`, `denominator`, `subgroup`, `subgroup_description`) but no `query`. Queries come from the member templates at dashboard-gen time.
- Bridge KPIs that don't exist on one side simply aren't listed in the bridge file. Bridge files are **curated** — only the cross-service KPIs the author cares about appear.

## Architecture

### TemplateRegistry — bridge storage and lookup

`crates/dashboard/src/service_extension.rs` adds:

```rust
pub struct BridgeExtension {
    pub service_name: String,
    pub members: [String; 2],
    pub kpis: Vec<BridgeKpi>,
}

pub struct BridgeKpi {
    pub role: String,
    pub title: String,
    pub metric_type: String,
    // ... same chart-spec fields as Kpi, minus `query`
    pub member_titles: HashMap<String, String>,  // member_name -> source_title
}

impl TemplateRegistry {
    pub fn find_bridge(&self, member_a: &str, member_b: &str) -> Option<&BridgeExtension> {
        self.bridges.values().find(|b| {
            let m = &b.members;
            (m[0] == member_a && m[1] == member_b) || (m[0] == member_b && m[1] == member_a)
        })
    }
}
```

Existing `services: HashMap<String, ServiceExtension>` stays. New `bridges: HashMap<String, BridgeExtension>` lives alongside. The disk loader (`load`) and embedded loader (`from_embedded`) both inspect the parsed JSON's `bridge` field to decide which map to insert into. Aliases on bridge files are not supported (YAGNI).

### Plot — new wire-format field

`crates/dashboard/src/plot.rs` adds one optional field on `Plot`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
promql_query_experiment: Option<String>,
```

Non-bridge plots leave it `None` (omitted from the JSON wire format). Bridge plots set it to the experiment-side query.

### Dashboard generation — new bridge generator

A new module `crates/dashboard/src/dashboard/bridge.rs` mirrors `service.rs` but takes:

- `&BridgeExtension`
- `&ServiceExtension` for baseline member
- `&ServiceExtension` for experiment member
- the baseline/experiment member names (so it knows which side of `member_titles` to look up)

For each bridge KPI:

1. Look up the baseline member's `Kpi` whose `title` equals `member_titles[baseline_member]` (defaulting to bridge KPI's own `title` when the entry is absent).
2. Look up the experiment member's `Kpi` the same way.
3. If either lookup fails, the bridge KPI is **skipped** and an entry appended to a `bridge_unavailable` array (`{ title, missing_member }`) which lands in section metadata.
4. Otherwise emit a `Plot` whose:
   - chart spec (id, title, role, type, unit_system, percentiles, full_width, denominator, subgroup) comes from the bridge KPI
   - `promql_query` is the baseline member KPI's `effective_query()` (handles histogram/counter wrapping)
   - `promql_query_experiment` is the experiment member KPI's `effective_query()`. Set to `None` when both are byte-identical (avoids unnecessary wire bytes and keeps the existing same-query A/B path).

The generator returns a `View` whose `metadata` includes:

- `service_name = bridge.service_name`
- `bridge_members = [baseline_member, experiment_member]`
- `bridge_unavailable` (when non-empty)

### Hook into existing dashboard generation

`dashboard::dashboard::generate(&Tsdb, filesize, &service_refs, ...)` currently emits per-service sections via `service::generate(...)`. Two changes:

1. The caller (server `viewer/mod.rs::run` / WASM `init_templates`) detects compare mode + bridge: when `service_refs.len() == 2 && registry.find_bridge(refs[0].0, refs[1].0).is_some()`, it calls `bridge::generate(...)` once instead of `service::generate(...)` twice, and substitutes that section into the section list.
2. In bridge mode, the per-member sections are **omitted** from the section list — they don't appear in the sidebar or get returned by `/data/<section>.json`.

Single-capture and same-source-compare paths fall through to the existing per-service generator unchanged.

### Server viewer — regenerate on attach

`src/viewer/mod.rs::attach_experiment` already triggers a dashboard regeneration via the existing service-extension detection path. The regeneration path consults `find_bridge` first; if a bridge applies, it routes through `bridge::generate`. On `detach_experiment`, the regeneration path naturally falls back to the single-capture per-service section.

### WASM viewer — regenerate on attach

`crates/viewer/src/lib.rs::WasmCaptureRegistry::attach_experiment` already calls `init_templates` after attaching. `init_templates` extends to call `find_bridge` and route to `bridge::generate` when applicable. Detach mirrors server behavior.

### Frontend — single change to `CompareChartWrapper`

`src/viewer/assets/lib/viewer_core.js`'s `fetchExperimentResult` currently builds the experiment query via:

```js
const query = buildEffectiveQuery(spec, { sectionRoute, crossCapture: true });
```

Extends to:

```js
const baseQuery = spec.promql_query_experiment || spec.promql_query;
const query = buildEffectiveQuery(
    { ...spec, promql_query: baseQuery },
    { sectionRoute, crossCapture: true },
);
```

That's it. The renderer (line/scatter overlay, side-by-side heatmap pair) doesn't care that queries differ — it sees two capture results, builds the A/B view exactly as it does for rezolus systems metrics.

The baseline path (`processDashboardData`'s per-plot `executePromQLRangeQuery`) is unchanged because `spec.promql_query` already holds the baseline member's query.

### Frontend — section header and unavailable list

`renderServiceSection` (in `src/viewer/assets/lib/service.js`) reads `meta.service_name` for the section title. It extends to optionally append `bridge_members` when present, e.g. `"Inference Library — vLLM vs SGLang"`. The existing "Unavailable KPIs" list at the bottom of the section is reused — `bridge_unavailable` entries render alongside the existing `unavailable_kpis` with their own sub-heading: *"Bridge skipped: 'TTFT' — no matching KPI in sglang."*

### Sidebar

Naturally derived from the section list returned by the dashboard. In bridge mode, the sidebar shows one `/service/inference-library` entry instead of two member entries. No frontend code change.

## Data flow (compare mode, bridge active)

1. User attaches two captures with sources `vllm` and `sglang`.
2. Server-side or WASM-side handler runs:
   ```
   service_refs = [("vllm", &vllm_ext), ("sglang", &sglang_ext)]
   registry.find_bridge("vllm", "sglang") → Some(&inference_library_bridge)
   sections = [bridge::generate(&inference_library_bridge, &vllm_ext, &sglang_ext, "vllm", "sglang"), ...rezolus sections]
   ```
3. Sidebar lists `Inference Library` (and the rezolus sections like `/cpu`, `/scheduler`, etc.).
4. Navigating to `/service/inference-library` returns the bridge section JSON. Each plot has:
   - `promql_query`: the vLLM-side query (baseline)
   - `promql_query_experiment`: the SGLang-side query
5. Frontend renders via the existing compare path. `CompareChartWrapper.fetchExperimentResult` reads `promql_query_experiment` and uses it for the experiment fetch through the metriken-query engine bound to the SGLang capture.
6. Both series populate; A/B overlay renders.

## Loader-time validation

At registry load:

- `bridge: true` files with `members.length !== 2` reject with `Failed to parse <path>: bridge must have exactly 2 members`.
- `member_titles` whose keys aren't a subset of `members` reject similarly.
- Member references that don't resolve to a loaded service template emit a warning and the bridge is dropped from the registry (so a bridge file pointing at a not-yet-present template doesn't break the registry, just silently doesn't activate).

## Testing

Unit tests in `crates/dashboard`:

- Parse a sample bridge JSON, assert `bridge: true` routes to `bridges` map and the members validation triggers on bad input.
- `find_bridge("vllm", "sglang")` and `find_bridge("sglang", "vllm")` return the same bridge.
- `bridge::generate(...)` with both members present produces N plots with both `promql_query` and `promql_query_experiment` set.
- A bridge KPI whose member-title misses on one side lands in `bridge_unavailable` and is excluded from the plot list.

Integration smoke (manual):

- `rezolus view vllm_gemma3.parquet sglang_gemma3.parquet` → sidebar shows "Inference Library", `/service/inference-library` route renders A/B overlays for the bridged KPIs, the rezolus systems sections still render normally.
- Static-site equivalent: `?capture=vllm_gemma3.parquet&capture=sglang_gemma3.parquet` against the deployed viewer reproduces the same shape.
- Single-capture (`?demo=vllm_gemma3.parquet`) still shows the regular `/service/vllm` section. No bridge involvement.
- Capture order swap (`?capture=sglang_gemma3.parquet&capture=vllm_gemma3.parquet`) renders the same bridge section with the labels and queries swapped (sglang as baseline, vllm as experiment).

## Files touched

| File | Change |
|---|---|
| `crates/dashboard/src/service_extension.rs` | Add `BridgeExtension`, `BridgeKpi`. Update `TemplateRegistry` to load bridges into a separate map. Add `find_bridge`. Validation for member counts and member_titles. |
| `crates/dashboard/src/dashboard/bridge.rs` | New module: `generate(&BridgeExtension, &ServiceExtension, &ServiceExtension, &str, &str) -> View`. |
| `crates/dashboard/src/dashboard/mod.rs` | Wire bridge generator into the public `generate(...)` entry, replacing per-member section generation when a bridge applies. |
| `crates/dashboard/src/plot.rs` | Add optional `promql_query_experiment` field on `Plot`. |
| `src/viewer/mod.rs` | Detect bridge in compare mode, route to `bridge::generate`. |
| `crates/viewer/src/lib.rs` | Same in `init_templates` for the WASM viewer. |
| `src/viewer/assets/lib/viewer_core.js` | One-line change in `fetchExperimentResult` to honor `spec.promql_query_experiment`. |
| `src/viewer/assets/lib/service.js` | Surface `bridge_members` in the section header; extend the unavailable list to render `bridge_unavailable`. |
| `config/templates/inference-library.json` | New bridge file joining `vllm` and `sglang`. |

## Risks & open questions

- **Unit/format mismatch across members.** If vLLM emits TTFT in seconds and SGLang in milliseconds, a bridge today will render both on the same Y-axis with no warning. Out of v1 scope; mention in the bridge author guide that members should agree on units. Future enhancement: optional `unit_conversion` per member in the bridge KPI.
- **Histogram percentile alignment.** A bridge KPI declares its own `percentiles`. The two member templates may declare different percentile sets. The bridge's `percentiles` wins (passed through `effective_query()` for both sides). If a member's underlying histogram doesn't store fine-enough buckets, percentiles may be coarse — same caveat as today's percentile rendering.
- **Bridge file load order.** Bridges reference members by `service_name`. If a bridge loads before its members, the validation either runs after all loads (preferred) or warns and drops. Spec mandates: registry collects all parsed files into temporary lists, validates bridges last after services are known.
- **Static-site bundling.** WASM viewer's templates list is hand-curated in `site/viewer/lib/script.js`. Adding `inference-library.json` requires updating that list (one-line append). Bridge templates download alongside service templates with no special handling.
