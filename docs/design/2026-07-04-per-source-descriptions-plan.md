# Per-Source Descriptions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Store metric descriptions per-source (`per_source_metadata.<source>.descriptions`) in combined parquets instead of a lossy union-merged top-level dict, and resolve them per-source-first with a top-level fallback in the viewer.

**Architecture:** The only combined-file producer is `combine.rs` (multi-endpoint `record` routes through `combine::combine_files`, recorder/mod.rs:975), so the producer change is confined there. Consumers gain a shared `resolve_descriptions` helper in the `dashboard` crate used by both the server handler and the WASM viewer (keeping their JSON byte-identical). Single-source `record` and existing parquets are untouched — the fallback covers them.

**Tech Stack:** Rust (serde_json, parquet), `cargo test`.

## Global Constraints

- Single-source `record` keeps writing the top-level `descriptions` key — do NOT change it.
- Read path is per-source-first, top-level fallback — no migration of existing parquets.
- Server (`/api/v1/metrics`) and WASM (`metrics()`) must produce byte-identical JSON — both use the same `resolve_descriptions` helper.
- `assemble_catalog`'s signature is unchanged (still takes a flat `{name→help}` map).

---

## Task 1: Producer — combine nests descriptions per-source

**Files:**
- Modify: `src/parquet_metadata.rs` (add `NESTED_DESCRIPTIONS` const + doc, near `NESTED_SAMPLER_STATUS` at line ~95)
- Modify: `src/parquet_tools/combine.rs` (descriptions block ~986‑1008, and the per-source build loop ~1012‑1095)
- Test: `#[cfg(test)]` in `src/parquet_tools/combine.rs`
- Docs: `CLAUDE.md` "Parquet File Format" section

**Interfaces:**
- Produces: parquet output where each source's descriptions live at
  `per_source_metadata.<source>.descriptions = {name→help}`; no top-level
  `descriptions` key in combined output.

- [ ] **Step 1: Add the schema constant**

In `src/parquet_metadata.rs` after `NESTED_SAMPLER_STATUS`:

```rust
/// Per-source metric descriptions (metric name → help text). Nested under
/// `per_source_metadata.<source>` in combined files; single-source files use
/// the top-level `descriptions` key instead.
pub const NESTED_DESCRIPTIONS: &str = "descriptions";
```

- [ ] **Step 2: Write the failing test**

Add to combine.rs tests. First inspect the existing combine tests to see how they obtain input parquets (grep `fn ` in the `#[cfg(test)]` module and any helper that writes a temp parquet with metadata). Use that same mechanism to build two single-source inputs, each with a top-level `descriptions` containing the SAME metric name with DIFFERENT text (source `a` → `{"m":"desc A"}`, source `b` → `{"m":"desc B"}`), combine them, and assert on the output footer metadata:

```rust
#[test]
fn combine_nests_descriptions_per_source_no_collision() {
    // ... build input_a (source "a", descriptions {"m":"desc A"}) and
    //     input_b (source "b", descriptions {"m":"desc B"}) via the existing
    //     test-fixture helper; combine to a temp output ...
    let psm: serde_json::Value = /* parse per_source_metadata from output */;
    assert_eq!(psm["a"]["descriptions"]["m"], "desc A");
    assert_eq!(psm["b"]["descriptions"]["m"], "desc B");
    // top-level descriptions is gone for combined output
    assert!(/* output has no top-level `descriptions` KV */);
}
```

> Implementer note: if the existing combine tests operate on real parquet files via a helper, reuse it. If there is no lightweight helper and constructing input parquets inline is heavy, extract the descriptions-nesting logic into a small pure function `fn nest_descriptions(per_source: &mut Map, source: &str, descriptions: Value)` and unit-test THAT directly, plus keep one integration assertion. Fill the test body with concrete asserts before implementing.

- [ ] **Step 3: Run the test — expect FAIL**

Run: `cargo test -p rezolus --lib combine_nests_descriptions 2>&1 | tail -20`
Expected: FAIL (descriptions still union-merged to top-level; `psm["a"]["descriptions"]` missing).

- [ ] **Step 4: Implement**

In `combine.rs`:
1. **Remove** the top-level union-merge block (~986‑1008: `merged_descriptions` + the `KEY_DESCRIPTIONS` push).
2. In the per-source build loop (where each input's `source_names` are known and `per_source.entry(source_name)` is populated), read that input's top-level `descriptions` KV once:

```rust
let input_desc: Option<serde_json::Value> = input
    .metadata
    .iter()
    .find(|kv| kv.key == KEY_DESCRIPTIONS)
    .and_then(|kv| kv.value.as_ref())
    .and_then(|s| serde_json::from_str(s).ok());
```

   and for each `source_name` of the input, attach it at the SOURCE level (sibling of the node/instance sub-keys):

```rust
if let Some(ref desc) = input_desc {
    let source_group = per_source
        .entry(source_name.clone())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(map) = source_group.as_object_mut() {
        map.entry(parquet_metadata::NESTED_DESCRIPTIONS.to_string())
            .or_insert_with(|| desc.clone());
    }
}
```

3. When carrying through an **already-combined** input's existing `per_source_metadata` (the merge block ~1020‑1057), the nested `descriptions` under each source come along automatically with the existing `target_map.entry(k).or_insert(v)` merge — verify no code strips it.

- [ ] **Step 5: Run the test — expect PASS**

Run: `cargo test -p rezolus --lib combine_nests_descriptions 2>&1 | tail -20`
Expected: PASS. Also run the full combine test module: `cargo test -p rezolus --lib parquet_tools::combine 2>&1 | tail -20` — all green.

- [ ] **Step 6: Update the format docs**

In `CLAUDE.md`, the "Parquet File Format" → `per_source_metadata` bullet: add `descriptions` (metric name → help) to the listed nested keys, and note single-source files keep the top-level `descriptions`.

- [ ] **Step 7: Commit**

```bash
git add src/parquet_metadata.rs src/parquet_tools/combine.rs CLAUDE.md
git commit -m "feat(parquet): nest descriptions per-source in combined files"
```

---

## Task 2: Consumer — `resolve_descriptions` helper + wire both handlers

**Files:**
- Modify: `crates/dashboard/src/metric_catalog.rs` (add `resolve_descriptions` + test)
- Modify: `src/viewer/routes.rs` (metrics_handler, ~286‑291)
- Modify: `crates/viewer/src/lib.rs` (`metrics()`, ~224‑229)

**Interfaces:**
- Consumes: `NESTED_DESCRIPTIONS` (Task 1).
- Produces:
  `pub fn resolve_descriptions(file_metadata: &serde_json::Value, source: &str) -> serde_json::Map<String, serde_json::Value>`
  — returns `per_source_metadata.<source>.descriptions` if present, else the
  top-level `descriptions`, else empty.

- [ ] **Step 1: Write the failing test**

In `crates/dashboard/src/metric_catalog.rs` `#[cfg(test)]`:

```rust
#[test]
fn resolve_descriptions_prefers_per_source_then_top_level() {
    let meta = serde_json::json!({
        "descriptions": { "m": "top" },
        "per_source_metadata": { "svc": { "descriptions": { "m": "per-source" } } }
    });
    assert_eq!(resolve_descriptions(&meta, "svc")["m"], "per-source");
    // source without a per-source entry -> top-level fallback
    assert_eq!(resolve_descriptions(&meta, "other")["m"], "top");
    // neither present -> empty
    let empty = serde_json::json!({});
    assert!(resolve_descriptions(&empty, "svc").is_empty());
}
```

- [ ] **Step 2: Run the test — expect FAIL**

Run: `cargo test -p dashboard resolve_descriptions 2>&1 | tail -20`
Expected: FAIL (`resolve_descriptions` not found).

- [ ] **Step 3: Implement the helper**

In `crates/dashboard/src/metric_catalog.rs`:

```rust
/// Resolve the effective descriptions map for `source`: the per-source
/// `per_source_metadata.<source>.descriptions` if present, else the legacy
/// top-level `descriptions`, else empty.
pub fn resolve_descriptions(
    file_metadata: &serde_json::Value,
    source: &str,
) -> serde_json::Map<String, serde_json::Value> {
    file_metadata
        .get("per_source_metadata")
        .and_then(|p| p.get(source))
        .and_then(|s| s.get("descriptions"))
        .and_then(|d| d.as_object())
        .cloned()
        .or_else(|| {
            file_metadata
                .get("descriptions")
                .and_then(|d| d.as_object())
                .cloned()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 4: Run the test — expect PASS**

Run: `cargo test -p dashboard resolve_descriptions 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Wire into the server handler**

In `src/viewer/routes.rs` `metrics_handler`, resolve the source first, then use the helper. Replace the current `descriptions` block (~286‑296) with:

```rust
let source = p.source.clone().unwrap_or_else(|| data.source());
let descriptions = state
    .captures
    .file_metadata(capture_id)
    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    .map(|v| dashboard::metric_catalog::resolve_descriptions(&v, &source))
    .unwrap_or_default();
let metrics = dashboard::metric_catalog::assemble_catalog(
    data.as_ref(), &descriptions, p.source.as_deref(),
);
let body = dashboard::metric_catalog::MetricsResponse { source, metrics };
```

- [ ] **Step 6: Wire into the WASM viewer**

In `crates/viewer/src/lib.rs` `metrics()` (~223‑234), build a metadata `Value` from the two keys `self.file_metadata` holds and call the helper. `self.file_metadata` is `HashMap<String, String>` where each value is that key's raw JSON string:

```rust
let resolved_source = source.clone().unwrap_or_else(|| self.reader.source());
let meta = serde_json::json!({
    "descriptions": self.file_metadata.get("descriptions")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
    "per_source_metadata": self.file_metadata.get("per_source_metadata")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
});
let descriptions = dashboard::metric_catalog::resolve_descriptions(&meta, &resolved_source);
let metrics = dashboard::metric_catalog::assemble_catalog(
    self.reader.as_ref(), &descriptions, source.as_deref(),
);
```

> Implementer note: confirm `self.file_metadata`'s exact type/representation (grep its declaration in lib.rs). If `per_source_metadata`'s value is already a parsed object rather than a JSON string, drop the inner `from_str`. The helper only needs a `Value` whose `per_source_metadata`/`descriptions` are objects.

- [ ] **Step 7: Build both crates**

Run: `cargo build -p rezolus 2>&1 | tail -5 && cargo check -p viewer --target wasm32-unknown-unknown 2>&1 | tail -5`
Expected: both clean.

- [ ] **Step 8: Commit**

```bash
git add crates/dashboard/src/metric_catalog.rs src/viewer/routes.rs crates/viewer/src/lib.rs
git commit -m "feat(viewer): resolve descriptions per-source with top-level fallback"
```

---

## Task 3: `mcp describe-metrics` fallback (verify, then fix or defer)

**Files:**
- Inspect: `src/mcp/describe_metrics.rs`

- [ ] **Step 1: Check** whether `describe_metrics` reads the top-level `descriptions` and would therefore show blank/collided descriptions on a combined file. Grep it for `descriptions` / `per_source_metadata`.
- [ ] **Step 2:** If it reads top-level only, give it the same `resolve_descriptions` fallback (it now lives in `dashboard::metric_catalog`), scoped per source where it iterates sources. If describe-metrics isn't source-scoped or the change is non-trivial, note it as a follow-up in the commit message / here rather than expanding scope.
- [ ] **Step 3: Commit** (or record the deferral).

---

## Self-Review

- **Spec coverage:** schema constant + producer nest (Task 1), consumer resolution + both handlers (Task 2), mcp check (Task 3), docs (Task 1 step 6), tests (Task 1 & 2). Single-source untouched (no task changes it). ✓
- **Known unverified seams (flagged, not placeholders):** the combine test-fixture mechanism (Task 1 step 2) and `self.file_metadata`'s exact representation (Task 2 step 6) — each has a concrete verification note.
- **Type consistency:** `resolve_descriptions(&Value, &str) -> Map` used identically in both handlers; `NESTED_DESCRIPTIONS` used in producer + (implicitly) matched by the `"descriptions"` key the helper reads.
