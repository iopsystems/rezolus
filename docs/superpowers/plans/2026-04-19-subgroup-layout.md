# Subgroup Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let dashboard authors express semantic subgroups within a `Group` and force a plot to span the full row, without CSS injection.

**Architecture:** `Group` gains `subgroups: Vec<SubGroup>`; `SubGroup` owns `Vec<Plot>` plus optional name and description. `Plot` gains a `width: PlotWidth` field (`Half` default, elided from JSON; `Full` renders `grid-column: 1 / -1`). Existing `Group::plot_promql*` methods append to a lazily-created default unnamed subgroup, so call sites compile unchanged. Viewer JS walks `subgroups` and accepts a compat shim for the legacy `plots` shape during the transition window.

**Tech Stack:** Rust (serde, metriken-query), Mithril.js, CSS Grid.

**Spec:** [docs/superpowers/specs/2026-04-19-subgroup-layout-design.md](../specs/2026-04-19-subgroup-layout-design.md).

---

## File Structure

**Files to modify (all relative to repo root):**

- `crates/dashboard/src/plot.rs` — Add `PlotWidth`, add `width` field to `Plot`, add `SubGroup`, refactor `Group` to own `Vec<SubGroup>`, add tests.
- `src/viewer/assets/lib/viewer_core.js` — Update Group component to walk `subgroups`, render optional name + description, wire per-plot `full-width` class. Keep a compat shim that promotes legacy `plots` to an unnamed subgroup.
- `src/viewer/assets/lib/style.css` — Add `.subgroup`, `.subgroup-title`, `.subgroup-description` rules; add `.chart-wrapper.full-width` rule.
- `src/viewer/assets/lib/layout.js` — Update plot-count traversal at line 142 to walk `group.subgroups[*].plots`.

**Symlinked assets:** `site/viewer/lib/` is a set of symlinks to `src/viewer/assets/lib/` (per CLAUDE.md). Changes to files under `src/viewer/assets/lib/` flow through automatically; nothing extra to edit.

**WASM viewer:** `crates/viewer/` consumes the same JS; no Rust changes needed beyond the shared `dashboard` crate that it already depends on transitively.

**Files explicitly NOT modified:**

- `crates/dashboard/src/dashboard/*.rs` — Existing per-section dashboard builders use `Group::plot_promql` and stay compiling via the backward-compat shim. No mass rewrite in this plan.

---

## Task 1: Add `PlotWidth` enum and `width` field on `Plot`

**Files:**
- Modify: `crates/dashboard/src/plot.rs` (near the existing `MetricType` enum around line 170, and the `Plot` struct around line 150)

- [ ] **Step 1: Write the failing test**

Append to the bottom of `crates/dashboard/src/plot.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_plot(width: PlotWidth) -> Plot {
        Plot {
            data: Vec::new(),
            opts: PlotOpts::counter("t", "id", Unit::Count),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
            promql_query: Some("up".into()),
            width,
        }
    }

    #[test]
    fn plot_width_half_is_elided_from_json() {
        let plot = make_plot(PlotWidth::Half);
        let json = serde_json::to_value(&plot).unwrap();
        assert!(
            json.get("width").is_none(),
            "expected `width` to be omitted when Half, got {json}"
        );
    }

    #[test]
    fn plot_width_full_is_serialized() {
        let plot = make_plot(PlotWidth::Full);
        let json = serde_json::to_value(&plot).unwrap();
        assert_eq!(json["width"], serde_json::json!("full"));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p dashboard plot_width 2>&1 | tail -20
```

Expected: compilation error — `PlotWidth` and `width` don't exist yet.

- [ ] **Step 3: Add `PlotWidth` enum and `width` field**

Edit `crates/dashboard/src/plot.rs`. After the `MetricType` enum add:

```rust
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlotWidth {
    Half,
    Full,
}

impl Default for PlotWidth {
    fn default() -> Self {
        PlotWidth::Half
    }
}

fn plot_width_is_half(w: &PlotWidth) -> bool {
    matches!(w, PlotWidth::Half)
}
```

Then extend the `Plot` struct with the new field. Locate the struct (around line 150) and add `width` at the end:

```rust
#[derive(Serialize, Clone)]
pub struct Plot {
    data: Vec<Vec<f64>>,
    opts: PlotOpts,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_data: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    formatted_time_data: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_names: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    promql_query: Option<String>,
    #[serde(skip_serializing_if = "plot_width_is_half", default)]
    pub width: PlotWidth,
}
```

Update the existing construction sites inside `plot_promql_with_descriptions` (around line 140) to include `width: PlotWidth::default()` in the `Plot { ... }` literal.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p dashboard plot_width 2>&1 | tail -20
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/plot.rs
git commit -m "feat(dashboard): add PlotWidth with Half/Full variants"
```

---

## Task 2: Add `SubGroup` struct

**Files:**
- Modify: `crates/dashboard/src/plot.rs`

- [ ] **Step 1: Write the failing test**

Inside the existing `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn subgroup_serializes_with_optional_name_and_description() {
    let sg = SubGroup {
        name: Some("Operations".into()),
        description: Some("Summary + per-device IOPS.".into()),
        plots: vec![],
    };
    let json = serde_json::to_value(&sg).unwrap();
    assert_eq!(json["name"], "Operations");
    assert_eq!(json["description"], "Summary + per-device IOPS.");
    assert_eq!(json["plots"], serde_json::json!([]));
}

#[test]
fn subgroup_elides_missing_name_and_description() {
    let sg = SubGroup {
        name: None,
        description: None,
        plots: vec![],
    };
    let json = serde_json::to_value(&sg).unwrap();
    assert!(json.get("name").is_none());
    assert!(json.get("description").is_none());
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p dashboard subgroup 2>&1 | tail -20
```

Expected: compilation error — `SubGroup` doesn't exist.

- [ ] **Step 3: Add the `SubGroup` struct**

In `crates/dashboard/src/plot.rs`, above the existing `Group` definition (around line 95), add:

```rust
#[derive(Serialize, Default)]
pub struct SubGroup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub plots: Vec<Plot>,
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p dashboard subgroup 2>&1 | tail -20
```

Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/plot.rs
git commit -m "feat(dashboard): add SubGroup struct with optional name and description"
```

---

## Task 3: Refactor `Group` to own `Vec<SubGroup>` with backward-compat shims

**Files:**
- Modify: `crates/dashboard/src/plot.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the `mod tests` block:

```rust
#[test]
fn legacy_plot_promql_creates_single_unnamed_subgroup() {
    let mut g = Group::new("G", "g");
    g.plot_promql(
        PlotOpts::counter("t1", "id1", Unit::Count),
        "up".into(),
    );
    g.plot_promql(
        PlotOpts::counter("t2", "id2", Unit::Count),
        "up".into(),
    );
    let json = serde_json::to_value(&g).unwrap();
    let subs = json["subgroups"].as_array().expect("subgroups present");
    assert_eq!(subs.len(), 1, "legacy calls collapse to one subgroup");
    assert!(subs[0].get("name").is_none(), "default subgroup is unnamed");
    assert_eq!(
        subs[0]["plots"].as_array().unwrap().len(),
        2,
        "both legacy plots land in the default subgroup"
    );
}

#[test]
fn group_no_longer_exposes_bare_plots_in_json() {
    let g = Group::new("G", "g");
    let json = serde_json::to_value(&g).unwrap();
    assert!(
        json.get("plots").is_none(),
        "Group JSON should expose subgroups, not plots"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p dashboard -- legacy_plot_promql group_no_longer 2>&1 | tail -20
```

Expected: `legacy_plot_promql_creates_single_unnamed_subgroup` fails because the current `Group` still serializes a flat `plots` array; `group_no_longer_exposes_bare_plots_in_json` also fails for the same reason.

- [ ] **Step 3: Refactor `Group` to own subgroups**

Replace the existing `Group` struct and `impl` block in `crates/dashboard/src/plot.rs` (around lines 87–150) with:

```rust
#[derive(Serialize)]
pub struct Group {
    name: String,
    id: String,
    subgroups: Vec<SubGroup>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Group {
    pub fn new<T: Into<String>, U: Into<String>>(name: T, id: U) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            subgroups: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Ensures a trailing subgroup exists to append plots to. Used by
    /// legacy `Group::plot_promql*` call sites so they keep working
    /// without conversion — the first legacy call opens a default
    /// unnamed subgroup, subsequent legacy calls append to the most
    /// recently opened subgroup.
    fn tail_subgroup_mut(&mut self) -> &mut SubGroup {
        if self.subgroups.is_empty() {
            self.subgroups.push(SubGroup::default());
        }
        self.subgroups.last_mut().unwrap()
    }

    /// Open a named subgroup. Returns a mutable reference so the
    /// caller can chain plot calls on it.
    pub fn subgroup<T: Into<String>>(&mut self, name: T) -> &mut SubGroup {
        self.subgroups.push(SubGroup {
            name: Some(name.into()),
            ..SubGroup::default()
        });
        self.subgroups.last_mut().unwrap()
    }

    /// Open an unnamed subgroup. Use when you want the "break to a new
    /// vertical band" effect without a visible header.
    pub fn subgroup_unnamed(&mut self) -> &mut SubGroup {
        self.subgroups.push(SubGroup::default());
        self.subgroups.last_mut().unwrap()
    }

    /// Legacy: append a plot to the current (or default) subgroup.
    pub fn plot_promql(&mut self, opts: PlotOpts, promql_query: String) {
        self.tail_subgroup_mut().plot_promql(opts, promql_query);
    }

    /// Legacy: append a plot with description-autofill support.
    pub fn plot_promql_with_descriptions(
        &mut self,
        opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.tail_subgroup_mut()
            .plot_promql_with_descriptions(opts, promql_query, descriptions);
    }
}
```

Do NOT add `SubGroup::plot_promql*` in this task — that's Task 4. For Task 3, provide just enough stubs on `SubGroup` so the compile succeeds. Add this minimal `impl SubGroup` block right after the `SubGroup` struct from Task 2:

```rust
impl SubGroup {
    pub fn plot_promql(&mut self, opts: PlotOpts, promql_query: String) {
        self.plot_promql_with_descriptions(opts, promql_query, None);
    }

    pub fn plot_promql_with_descriptions(
        &mut self,
        mut opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        if opts.description.is_none()
            && let Some(descriptions) = descriptions
        {
            let mut best_match: Option<(usize, &str, &str)> = None;
            for (name, desc) in descriptions {
                if let Some(pos) = promql_query.find(name.as_str()) {
                    let dominated = best_match.is_some_and(|(best_pos, best_name, _)| {
                        name.len() < best_name.len()
                            || (name.len() == best_name.len()
                                && (pos > best_pos
                                    || (pos == best_pos && name.as_str() > best_name)))
                    });
                    if !dominated {
                        best_match = Some((pos, name.as_str(), desc.as_str()));
                    }
                }
            }
            if let Some((_, _, desc)) = best_match {
                opts.description = Some(desc.to_string());
            }
        }

        self.plots.push(Plot {
            opts,
            data: Vec::new(),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
            promql_query: Some(promql_query),
            width: PlotWidth::default(),
        });
    }
}
```

Delete the old body of `Group::plot_promql_with_descriptions` — it's been moved into `SubGroup`.

- [ ] **Step 4: Run the tests**

```bash
cargo test -p dashboard 2>&1 | tail -20
```

Expected: all `dashboard` crate tests pass, including the two new ones from Step 1 plus the pre-existing tests in `service_extension.rs` and `dashboard/mod.rs`.

- [ ] **Step 5: Verify all existing dashboard call sites compile**

```bash
cargo build -p dashboard 2>&1 | tail -20
```

Expected: clean build. If any call site in `crates/dashboard/src/dashboard/*.rs` fails, the error will be at a `group.plot_promql*` call — fix by updating the signature to match the new `Group` (signatures have NOT changed, so this shouldn't happen).

- [ ] **Step 6: Commit**

```bash
git add crates/dashboard/src/plot.rs
git commit -m "refactor(dashboard): Group owns Vec<SubGroup>; legacy plot_promql appends to default"
```

---

## Task 4: Add `SubGroup::plot_promql_full` and `SubGroup::describe`

**Files:**
- Modify: `crates/dashboard/src/plot.rs`

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests`:

```rust
#[test]
fn plot_promql_full_marks_plot_as_full_width() {
    let mut g = Group::new("G", "g");
    let sg = g.subgroup("Ops");
    sg.plot_promql_full(
        PlotOpts::counter("Summary", "sum", Unit::Count),
        "up".into(),
    );
    let json = serde_json::to_value(&g).unwrap();
    assert_eq!(
        json["subgroups"][0]["plots"][0]["width"],
        serde_json::json!("full")
    );
}

#[test]
fn describe_sets_the_description_field() {
    let mut g = Group::new("G", "g");
    g.subgroup("Ops").describe("Shows total throughput and IOPS.");
    let json = serde_json::to_value(&g).unwrap();
    assert_eq!(
        json["subgroups"][0]["description"],
        "Shows total throughput and IOPS."
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p dashboard -- plot_promql_full describe_sets 2>&1 | tail -20
```

Expected: both fail — methods don't exist.

- [ ] **Step 3: Add `plot_promql_full` and `describe` on `SubGroup`**

Extend the `impl SubGroup` block with:

```rust
    /// Set the optional description text rendered below the subgroup header.
    pub fn describe<T: Into<String>>(&mut self, text: T) -> &mut Self {
        self.description = Some(text.into());
        self
    }

    /// Append a plot that spans the full width of the group's grid.
    pub fn plot_promql_full(&mut self, opts: PlotOpts, promql_query: String) {
        self.plot_promql_full_with_descriptions(opts, promql_query, None);
    }

    /// Full-width variant with description autofill.
    pub fn plot_promql_full_with_descriptions(
        &mut self,
        opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.plot_promql_with_descriptions(opts, promql_query, descriptions);
        if let Some(plot) = self.plots.last_mut() {
            plot.width = PlotWidth::Full;
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p dashboard 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/plot.rs
git commit -m "feat(dashboard): SubGroup::describe and plot_promql_full for full-width charts"
```

---

## Task 5: Verify existing dashboard JSON still structurally matches the viewer's expectations

**Files:**
- Run-only: `crates/dashboard/src/main.rs` (the debug binary)

- [ ] **Step 1: Dump the dashboard JSON before/after comparison spot check**

```bash
cargo run -p dashboard 2>&1 | head -40
```

Expected: the output prints sections, and each `Group` JSON now shows:

```json
"groups": [
  {
    "name": "...",
    "id": "...",
    "subgroups": [
      {
        "plots": [ ... ]
      }
    ]
  }
]
```

Instead of the previous `"plots": [ ... ]` directly on the group. The wrapping subgroup has no `name` and no `description`. Confirm visually that no existing dashboard sections produced a `"plots"` key at the Group level.

- [ ] **Step 2: Confirm clippy is clean**

```bash
cargo clippy -p dashboard --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: no warnings.

- [ ] **Step 3: No commit needed** — this task is a verification gate.

---

## Task 6: Update the viewer's Group component to walk subgroups

**Files:**
- Modify: `src/viewer/assets/lib/viewer_core.js`

- [ ] **Step 1: Read the current Group component**

Read [viewer_core.js:13-70](src/viewer/assets/lib/viewer_core.js#L13-L70) to understand the current structure. The component iterates `attrs.plots.map(...)` and emits `div.chart-wrapper` inside `div.charts`.

- [ ] **Step 2: Update the component**

Replace the body of `createGroupComponent(getState)`'s `view({ attrs })` function. The new body walks `attrs.subgroups` (with a compat fallback to `attrs.plots`) and renders each subgroup's name + description + charts grid. Full-width plots get a `full-width` class.

Open `src/viewer/assets/lib/viewer_core.js` and replace the existing `return m('div.group', ...)` block (roughly lines 36-67) with:

```js
            // Compat shim: if the incoming JSON still uses the legacy
            // `plots` shape (an array directly on the group), promote it
            // to a single unnamed subgroup so rendering stays uniform.
            const subgroups = attrs.subgroups
                ? attrs.subgroups
                : [{ name: null, description: null, plots: attrs.plots || [] }];

            const renderChart = (spec) => {
                const isHistogramChart = isHistogramPlot(spec);
                const wrapperClass = spec.width === 'full'
                    ? 'div.chart-wrapper.full-width'
                    : 'div.chart-wrapper';

                if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                    const heatmapData = sectionHeatmapData.get(spec.opts.id);
                    const heatmapSpec = buildHistogramHeatmapSpec(spec, heatmapData, prefixTitle(spec.opts));
                    return m(wrapperClass, [
                        chartHeader(heatmapSpec.opts),
                        m(Chart, { spec: heatmapSpec, chartsState, interval }),
                        expandLink(spec, sectionRoute),
                        selectButton(spec, sectionRoute, sectionName),
                    ]);
                }

                const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse };
                return m(wrapperClass, [
                    chartHeader(prefixedSpec.opts),
                    m(Chart, { spec: prefixedSpec, chartsState, interval }),
                    expandLink(spec, sectionRoute),
                    selectButton(spec, sectionRoute, sectionName),
                ]);
            };

            return m(
                'div.group',
                { id: attrs.id },
                [
                    m('h2', `${attrs.name}`),
                    subgroups.map((sg) =>
                        m('div.subgroup', [
                            sg.name && m('h3.subgroup-title', sg.name),
                            sg.description && m('p.subgroup-description', sg.description),
                            m('div.charts', (sg.plots || []).map(renderChart)),
                        ])
                    ),
                ],
            );
```

Keep the existing lines 13-35 (`view({ attrs })` signature, state destructuring, `titlePrefix`/`prefixTitle`/`chartHeader`/`noCollapse` declarations) unchanged.

- [ ] **Step 3: Load a parquet file in the server viewer to verify visually**

```bash
cargo build --release 2>&1 | tail -5
./target/release/rezolus view data/demo.parquet 127.0.0.1:4242 &
SERVER_PID=$!
sleep 1
curl -s http://127.0.0.1:4242/overview | head -1
kill $SERVER_PID
```

(If `data/demo.parquet` doesn't exist, substitute any parquet recording you have locally. Ask the user if unsure.) Open `http://127.0.0.1:4242/` in a browser. Expected: all existing sections render exactly as before. Each chart sits in a `div.subgroup > div.charts` container that you can see in the DOM inspector. No charts should be missing or misplaced.

- [ ] **Step 4: Commit**

```bash
git add src/viewer/assets/lib/viewer_core.js
git commit -m "feat(viewer): Group component walks subgroups, with legacy plots shim"
```

---

## Task 7: Update `layout.js` plot-count traversal

**Files:**
- Modify: `src/viewer/assets/lib/layout.js`

- [ ] **Step 1: Read the current traversal**

```bash
sed -n '135,150p' src/viewer/assets/lib/layout.js
```

Note the function body that iterates `group.plots`.

- [ ] **Step 2: Update the traversal to walk subgroups**

Open `src/viewer/assets/lib/layout.js`. Replace the block currently at lines 137-146 (the plot-count loop) with:

```js
// Count plots with non-empty data across groups and their subgroups.
// Supports both shapes: new `group.subgroups[*].plots` and legacy
// `group.plots` (flat).
function collectPlots(group) {
    if (Array.isArray(group.subgroups)) {
        return group.subgroups.flatMap((sg) => sg.plots || []);
    }
    return group.plots || [];
}
```

Then at the existing iteration site, replace `for (const plot of group.plots || [])` with:

```js
for (const plot of collectPlots(group))
```

- [ ] **Step 3: Verify the viewer still loads**

Rebuild and reload the viewer in the browser; click through sections, verify the plot count is correct (e.g., the section header's chart count, if it's displayed).

```bash
cargo build --release 2>&1 | tail -5
./target/release/rezolus view data/demo.parquet 127.0.0.1:4242 &
SERVER_PID=$!
sleep 1
# Visit http://127.0.0.1:4242/ manually; verify no console errors
kill $SERVER_PID
```

- [ ] **Step 4: Commit**

```bash
git add src/viewer/assets/lib/layout.js
git commit -m "fix(viewer): layout plot count walks subgroups"
```

---

## Task 8: Add CSS for `.subgroup` and `.chart-wrapper.full-width`

**Files:**
- Modify: `src/viewer/assets/lib/style.css`

- [ ] **Step 1: Locate the existing `.group .charts` rule**

```bash
sed -n '940,960p' src/viewer/assets/lib/style.css
```

Confirm the rule at lines 947-949 is:

```css
.group .charts {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
}
```

- [ ] **Step 2: Add subgroup and full-width rules**

Insert the following immediately after the `.group .charts` block (before line 950 starts the next rule). Open `src/viewer/assets/lib/style.css` and add:

```css
.group .subgroup {
    margin-bottom: 1rem;
}
.group .subgroup:last-child {
    margin-bottom: 0;
}
.group .subgroup-title {
    font-size: 0.95rem;
    font-weight: 500;
    color: var(--fg-secondary);
    margin: 0 0 0.25rem 0;
}
.group .subgroup-description {
    font-size: 0.85rem;
    color: var(--fg-secondary);
    line-height: 1.4;
    margin: 0 0 0.5rem 0;
}
.group .charts .chart-wrapper.full-width {
    grid-column: 1 / -1;
}
```

The narrow-screen `@media (max-width: 1199px) { .group .charts { grid-template-columns: 1fr } }` rule at lines 2732-2736 already collapses to a single column, and `grid-column: 1 / -1` is a no-op there, so no extra media-query rule is required.

- [ ] **Step 3: Verify visually**

Rebuild the viewer and load a page. In the DOM inspector, confirm `.subgroup` wraps each chart cluster and that the spacing looks reasonable (compare to before the change — it should be nearly identical, with a small gap between subgroups if/when any appear).

- [ ] **Step 4: Commit**

```bash
git add src/viewer/assets/lib/style.css
git commit -m "feat(viewer): css for subgroup header, description, and full-width plots"
```

---

## Task 9: Adopt subgroups in one existing dashboard as a smoke test

**Files:**
- Modify: `crates/dashboard/src/dashboard/blockio.rs`

This task validates the end-to-end feature on a real dashboard section without doing a mass rewrite. We pick `blockio` because it has the natural "summary + per-device" pattern.

- [ ] **Step 1: Read the existing blockio Operations group**

Look at `crates/dashboard/src/dashboard/blockio.rs` lines 1–55 (from earlier exploration). The Operations group has 6 plots: Total Throughput, Total IOPS, Read Throughput, Read IOPS, Write Throughput, Write IOPS.

- [ ] **Step 2: Restructure Operations into subgroups**

Replace the existing `operations` group construction with:

```rust
    let mut operations = Group::new("Operations", "operations");

    let totals = operations.subgroup("Totals");
    totals.describe("Throughput and operation rate aggregated across all block devices.");
    totals.plot_promql(
        PlotOpts::counter(
            "Total Throughput",
            "blockio-throughput-total",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes[5m]))".to_string(),
    );
    totals.plot_promql(
        PlotOpts::counter("Total IOPS", "blockio-iops-total", Unit::Count),
        "sum(irate(blockio_operations[5m]))".to_string(),
    );

    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        let sg = operations.subgroup(*op);
        sg.plot_promql(
            PlotOpts::counter(
                format!("{op} Throughput"),
                format!("throughput-{op_lower}"),
                Unit::Datarate,
            ),
            format!("sum(irate(blockio_bytes{{op=\"{op_lower}\"}}[5m]))"),
        );
        sg.plot_promql(
            PlotOpts::counter(
                format!("{op} IOPS"),
                format!("iops-{op_lower}"),
                Unit::Count,
            ),
            format!("sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"),
        );
    }

    view.group(operations);
```

- [ ] **Step 3: Build and dump the JSON**

```bash
cargo run -p dashboard 2>&1 | grep -A 50 '"id": "blockio"' | head -80
```

Expected: the Operations group now has three subgroups (`Totals`, `Read`, `Write`), each with two plots. The `Totals` subgroup has a description.

- [ ] **Step 4: Load the viewer and visually verify**

Rebuild the release binary, load a parquet that contains block-io data, and open the BlockIO section. Expected: three distinct clusters — Totals (with its description text), Read, Write — each cluster showing its two plots side-by-side. The overall visual information density should feel similar to before, with more obvious semantic grouping.

- [ ] **Step 5: Commit**

```bash
git add crates/dashboard/src/dashboard/blockio.rs
git commit -m "feat(dashboard): restructure blockio Operations into Totals/Read/Write subgroups"
```

---

## Task 10: Final verification

**Files:** (verification only, no edits)

- [ ] **Step 1: Run full test suite and clippy**

```bash
cargo clippy --all-targets -- -D warnings 2>&1 | tail -20
cargo test 2>&1 | tail -20
```

Expected: both clean.

- [ ] **Step 2: Smoke-test the WASM viewer build**

```bash
./crates/viewer/build.sh 2>&1 | tail -20
```

Expected: successful build, writing to `site/viewer/pkg/`.

- [ ] **Step 3: Load the static site viewer locally**

```bash
cd site && python3 -m http.server 8080 &
SERVER_PID=$!
sleep 1
# Visit http://localhost:8080/viewer/ in a browser, drop a parquet,
# confirm dashboard renders with subgroups.
kill $SERVER_PID
cd ..
```

- [ ] **Step 4: No commit** — verification gate. The branch is ready to push/PR when all the above are green.

---

## Self-Review (ran before handoff)

- **Spec coverage:**
  - `PlotWidth` + `width` field — Task 1.
  - `SubGroup` with optional name and description — Task 2.
  - `Group.subgroups: Vec<SubGroup>` refactor with legacy `plot_promql*` delegation — Task 3.
  - Subgroup authoring API (`subgroup`, `subgroup_unnamed`, `describe`, `plot_promql`, `plot_promql_with_descriptions`, `plot_promql_full`) — Tasks 3 & 4.
  - Backward-compatibility shim at the Rust layer — Task 3.
  - Viewer Group component walks subgroups with compat shim — Task 6.
  - CSS for subgroup header, description, and full-width — Task 8.
  - Viewer plot-count traversal — Task 7.
  - Smoke adoption in one real dashboard — Task 9.
  - Empty-Tsdb safety — implicit via Task 5 JSON dump (debug binary uses `Tsdb::default()`).
  - Existing dashboards keep compiling — Task 3 Step 5.

- **Placeholder scan:** No `TBD`, no "implement later", every code step contains actual code; exact commands and file paths throughout.

- **Type consistency:** `PlotWidth` used the same across tasks; `SubGroup` field names (`name`, `description`, `plots`) match between struct definition and all test assertions; `subgroup`, `subgroup_unnamed`, `describe`, `plot_promql_full` names used consistently; viewer compat shim reads both `attrs.subgroups` and `attrs.plots` as specified.
