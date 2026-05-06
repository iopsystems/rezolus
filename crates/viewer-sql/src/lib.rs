//! WASM viewer skeleton driving duckdb-wasm via JS instead of the legacy
//! in-memory Tsdb + PromQL engine.
//!
//! Architecture (see `/home/yurivish/.claude/plans/we-re-interested-in-porting-lexical-squirrel.md`):
//!
//!   Browser
//!     ├─ AsyncDuckDB (worker-backed) — runs SQL, holds parquet
//!     ├─ JS host shim — boots duckdb, registers parquet + macros (the
//!     │   pure-SQL macros from /work/duckdb-prototyping/wasm-poc/), then
//!     │   asks the schema once and constructs ViewerSql with the conn
//!     │   handle + the schema metadata.
//!     └─ ViewerSql (this crate)
//!           ├─ Drives queries against the JS-side conn via JsFuture
//!           └─ Implements DashboardData via cached schema → reuses the
//!               same `dashboard::generate_section` generators that the
//!               legacy viewer uses
//!
//! This commit is intentionally a thin scaffold:
//!   - Crate compiles to wasm32 with the wasm-bindgen surface in place
//!   - SqlMetadata + DashboardData impl established
//!   - Async query helpers in place (via wasm_bindgen_futures::JsFuture)
//!   - Macros source-of-truth lives in this crate so the JS host can pull
//!     them via wasm-exposed `pure_sql_macros()` and CREATE them on boot
//!   - Section rendering wired through to dashboard::generate_section
//!
//! Future commits add: query_range, init_templates, compare-mode (paired
//! captures), and the full method surface that mirrors `crates/viewer`.

use dashboard::DashboardData;
use js_sys::{Function, Promise, Reflect};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// The pure-SQL macros that replace the canonical Rust UDFs. JS host calls
/// this once after boot and runs each statement against the AsyncDuckDB
/// connection. Source of truth for the macro layer lives here so the
/// macros version with the Rust crate.
///
/// Returns a single SQL script — JS splits on `;` boundaries (or runs as
/// `executeBatch` if available).
#[wasm_bindgen]
pub fn pure_sql_macros() -> String {
    // Inline because including a string from a separate file would require a
    // build script for wasm32. The canonical native UDFs live in
    // /work/metriken/metriken-query-sql/src/{udf,macros}.rs.
    const MACROS: &str = include_str!("macros.sql");
    MACROS.to_string()
}

/// Read-only snapshot of the loaded parquet's metadata. JS computes this
/// once at parquet-load time (running DESCRIBE + a couple of summary
/// queries against the AsyncDuckDB connection) and passes it to the
/// ViewerSql constructor.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SqlMetadata {
    pub interval_seconds: f64,
    /// (min, max) timestamps in nanoseconds; None for empty parquet.
    pub time_range_ns: Option<(u64, u64)>,
    pub source: String,
    pub version: String,
    pub filename: String,
    /// Name the parquet was registered under via `db.registerFileBuffer`.
    /// `query_range` references this in the `_src` CTE wrap. Defaults to the
    /// conventional `"capture.parquet"` if the JS host doesn't override it.
    #[serde(default = "default_parquet_name")]
    pub parquet_name: String,
    /// Counter metric name → number of distinct label sets.
    pub counters: HashMap<String, usize>,
    pub gauges: HashMap<String, usize>,
    pub histograms: HashMap<String, usize>,
}

fn default_parquet_name() -> String {
    "capture.parquet".to_string()
}

impl DashboardData for SqlMetadata {
    fn interval(&self) -> f64 {
        self.interval_seconds
    }
    fn time_range(&self) -> Option<(u64, u64)> {
        self.time_range_ns
    }
    fn source(&self) -> &str {
        &self.source
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn filename(&self) -> &str {
        &self.filename
    }
    fn counter_names(&self) -> Vec<&str> {
        self.counters.keys().map(String::as_str).collect()
    }
    fn gauge_names(&self) -> Vec<&str> {
        self.gauges.keys().map(String::as_str).collect()
    }
    fn histogram_names(&self) -> Vec<&str> {
        self.histograms.keys().map(String::as_str).collect()
    }
    fn counter_label_count(&self, name: &str) -> usize {
        self.counters.get(name).copied().unwrap_or(0)
    }
    fn gauge_label_count(&self, name: &str) -> usize {
        self.gauges.get(name).copied().unwrap_or(0)
    }
    fn histogram_label_count(&self, name: &str) -> usize {
        self.histograms.get(name).copied().unwrap_or(0)
    }
}

#[wasm_bindgen]
pub struct ViewerSql {
    /// JS handle to a duckdb-wasm AsyncDuckDBConnection. The conn is
    /// already booted, has the parquet registered, and has the macros
    /// from `pure_sql_macros()` registered.
    conn: JsValue,
    /// Schema/metadata snapshot the JS host computed during boot.
    metadata: SqlMetadata,
    /// Section navigation context. Populated by `init_templates` (single
    /// capture). When empty, `get_sections` returns "[]".
    context: dashboard::dashboard::DashboardContext,
    /// Memoized rendered section bodies, keyed by route stem.
    cached_bodies: RefCell<HashMap<String, serde_json::Value>>,
    /// Display alias for this capture, when the JS caller supplied one.
    alias: Option<String>,
}

#[wasm_bindgen]
impl ViewerSql {
    /// Construct from a JS-side conn handle and a metadata blob.
    ///
    /// Metadata arrives as a JSON string rather than a structured JS value
    /// because nanosecond timestamps exceed Number's 2^53 precision range,
    /// and serde-wasm-bindgen's default Number↔primitive bridge would
    /// silently corrupt them. JS host serializes BigInts to decimal strings
    /// inside the JSON; serde_json::Value::U64 parses decimal-string
    /// numerics losslessly.
    #[wasm_bindgen(constructor)]
    pub fn new(conn: JsValue, metadata_json: &str) -> Result<ViewerSql, JsValue> {
        let metadata: SqlMetadata = serde_json::from_str(metadata_json)
            .map_err(|e| JsValue::from_str(&format!("invalid metadata json: {e}")))?;
        let context = dashboard::dashboard::build_dashboard_context(None, &[], None);
        Ok(ViewerSql {
            conn,
            metadata,
            context,
            cached_bodies: RefCell::new(HashMap::new()),
            alias: None,
        })
    }

    pub fn set_alias(&mut self, alias: Option<String>) {
        self.alias = alias;
    }

    /// JSON metadata compatible with the legacy viewer's /api/v1/metadata.
    pub fn metadata(&self) -> String {
        let (min_time, max_time) = self
            .metadata
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));
        serde_json::json!({
            "status": "success",
            "data": {
                "minTime": min_time,
                "maxTime": max_time,
                "fileChecksum": "",
                "alias": self.alias,
            }
        })
        .to_string()
    }

    /// Capture-info JSON: interval, source, version, filename, time range,
    /// metric inventory. Same shape the legacy `Viewer::info()` emits.
    pub fn info(&self) -> String {
        let (min_time, max_time) = self
            .metadata
            .time_range()
            .map(|(min, max)| (min as f64 / 1e9, max as f64 / 1e9))
            .unwrap_or((0.0, 0.0));
        serde_json::json!({
            "interval": self.metadata.interval(),
            "source": self.metadata.source(),
            "version": self.metadata.version(),
            "filename": self.metadata.filename(),
            "minTime": min_time,
            "maxTime": max_time,
            "counter_names": self.metadata.counter_names(),
            "gauge_names": self.metadata.gauge_names(),
            "histogram_names": self.metadata.histogram_names(),
        })
        .to_string()
    }

    /// Section navigation list, JSON-serialized. Empty array until the JS
    /// host calls `init_templates` (not yet implemented).
    pub fn get_sections(&self) -> String {
        serde_json::to_string(&self.context.sections).unwrap_or_else(|_| "[]".to_string())
    }

    /// Render a single section by route stem (e.g. "cpu", "service/vllm").
    /// Returns `None` for unknown routes (JS treats as 404).
    pub fn get_section(&self, key: &str) -> Option<String> {
        if let Some(cached) = self.cached_bodies.borrow().get(key) {
            return Some(cached.to_string());
        }
        let route = format!("/{key}");
        let view = dashboard::dashboard::generate_section(&self.metadata, &route, &self.context)?;
        let value = serde_json::to_value(&view).ok()?;
        let s = value.to_string();
        self.cached_bodies.borrow_mut().insert(key.to_string(), value);
        Some(s)
    }

    /// Run `SELECT count(*) FROM read_parquet(<registered_name>)` against
    /// the JS-side conn. Returned as f64 to dodge BigInt marshalling for
    /// this simple sanity-check method. Used by the JS host as a smoke test.
    pub async fn count_rows(&self, parquet_name: String) -> Result<f64, JsValue> {
        let sql = format!(
            "SELECT count(*)::DOUBLE AS n FROM read_parquet('{}')",
            parquet_name
        );
        let table = self.query(&sql).await?;
        first_double_cell(&table)
    }

    /// Run a raw SQL string against the JS-side conn and return rows as a
    /// JSON-serialized array of objects. The shape mirrors arrow-js's
    /// `Table.toArray().map(r => r.toJSON())`. BigInt columns survive as
    /// decimal-string fields.
    ///
    /// This is the shim the dashboard frontend will eventually call
    /// through `query_range`/`query`. For now callers pass SQL directly;
    /// the PromQL→SQL translator lands in a follow-up.
    pub async fn query_sql(&self, sql: String) -> Result<String, JsValue> {
        let table = self.query(&sql).await?;
        arrow_table_to_json(&table)
    }

    /// Run a Phase D-shaped dashboard SQL string and return Prometheus
    /// matrix-shape JSON, the same shape `/api/v1/query_range` returns from
    /// the legacy viewer.
    ///
    /// The dashboard SQL must:
    ///   - Reference parquet columns via `_src` (not `read_parquet(...)` directly).
    ///   - Project a `t` (DOUBLE seconds) column and a `v` (numeric) column.
    ///   - Optionally project label columns (e.g. `id`, `state`) — each
    ///     becomes a `metric:{label: value}` entry in the result series.
    ///
    /// `step` is currently ignored; the frontend handles client-side step
    /// resampling. start/end are seconds-since-epoch; we convert to ns to
    /// filter `_src` at the source for cheaper window evaluation.
    pub async fn query_range(
        &self,
        sql: String,
        start: f64,
        end: f64,
        _step: f64,
    ) -> Result<String, JsValue> {
        let start_ns = (start * 1e9) as i64;
        let end_ns = (end * 1e9) as i64;
        let wrapped = format!(
            "WITH _src AS ( \
               SELECT * FROM read_parquet('{parquet}') \
               WHERE timestamp BETWEEN {start_ns} AND {end_ns} \
             ) \
             SELECT * FROM ({user_sql}) ORDER BY t",
            parquet = self.metadata.parquet_name,
            user_sql = sql,
        );
        let table = self.query(&wrapped).await?;
        arrow_table_to_prom_matrix(&table)
    }

    /// Lower-level: run a SQL string against the JS-side conn and return
    /// the Arrow Table as a JsValue. Internal helper for query methods
    /// that walk the result themselves.
    async fn query(&self, sql: &str) -> Result<JsValue, JsValue> {
        let query_fn: Function = Reflect::get(&self.conn, &"query".into())?.dyn_into()?;
        let promise: Promise = query_fn
            .call1(&self.conn, &JsValue::from_str(sql))?
            .dyn_into()?;
        JsFuture::from(promise).await
    }
}

/// Walk an Arrow JS Table to a JSON-encoded array-of-objects. Uses the
/// table's own `toArray()` + per-row `toJSON()` so column-type-specific
/// formatting (BigInt → string, lists → arrays) matches what arrow-js
/// itself produces — the same shape the legacy viewer's PromQL response
/// JSON wraps. BigInts are stringified to survive serde_json (no
/// arbitrary-precision integer type in JS Number).
fn arrow_table_to_json(table: &JsValue) -> Result<String, JsValue> {
    let to_array_fn: Function = Reflect::get(table, &"toArray".into())?.dyn_into()?;
    let rows = to_array_fn.call0(table)?;
    let length = Reflect::get(&rows, &"length".into())?
        .as_f64()
        .ok_or_else(|| JsValue::from_str("rows.length not a number"))? as usize;

    let json_stringify: Function = {
        let global = js_sys::global();
        let json = Reflect::get(&global, &"JSON".into())?;
        Reflect::get(&json, &"stringify".into())?.dyn_into()?
    };

    let mut out = String::with_capacity(64 + length * 32);
    out.push('[');
    for i in 0..length {
        if i > 0 {
            out.push(',');
        }
        let row = Reflect::get(&rows, &(i as u32).into())?;
        let to_json: Function = Reflect::get(&row, &"toJSON".into())?.dyn_into()?;
        let plain = to_json.call0(&row)?;
        // Custom replacer: BigInt → string. The replacer takes (key, value).
        let replacer = js_sys::Function::new_with_args(
            "k,v",
            "return typeof v === 'bigint' ? v.toString() : v;",
        );
        let s = json_stringify
            .call2(&JsValue::NULL, &plain, &replacer)?
            .as_string()
            .ok_or_else(|| JsValue::from_str("JSON.stringify did not return a string"))?;
        out.push_str(&s);
    }
    out.push(']');
    Ok(out)
}

/// Walk an Arrow JS Table and emit Prometheus matrix-shape JSON:
///   {status:"success", data:{resultType:"matrix", result:[
///     {metric: {<label>:<value>, ...}, values: [[t_seconds, v_string], ...]}
///   ]}}
///
/// Column-role detection from the table schema:
///   - Field named `t` → timestamp axis (DOUBLE seconds since epoch).
///   - Field named `v` → numeric value axis.
///   - All other fields → series labels. Their per-row values key the row
///     into a result series. NULL `v` rows are dropped (Prometheus series
///     gap semantics).
///
/// All non-`t`/`v` columns are stringified for the metric label dictionary.
fn arrow_table_to_prom_matrix(table: &JsValue) -> Result<String, JsValue> {
    // Inspect the schema to identify column roles.
    let schema = Reflect::get(table, &"schema".into())?;
    let fields = Reflect::get(&schema, &"fields".into())?;
    let n_fields = Reflect::get(&fields, &"length".into())?
        .as_f64()
        .ok_or_else(|| JsValue::from_str("schema.fields.length not a number"))?
        as usize;

    let mut t_idx: Option<usize> = None;
    let mut v_idx: Option<usize> = None;
    let mut label_indices: Vec<(usize, String)> = Vec::new();
    for i in 0..n_fields {
        let field = Reflect::get(&fields, &(i as u32).into())?;
        let name = Reflect::get(&field, &"name".into())?
            .as_string()
            .unwrap_or_default();
        match name.as_str() {
            "t" => t_idx = Some(i),
            "v" => v_idx = Some(i),
            _ => label_indices.push((i, name)),
        }
    }
    let t_idx = t_idx.ok_or_else(|| JsValue::from_str("query result missing required `t` column"))?;
    let v_idx = v_idx.ok_or_else(|| JsValue::from_str("query result missing required `v` column"))?;

    // Pre-fetch the column vectors by index — avoids per-row Reflect on the table.
    let get_child_at: Function = Reflect::get(table, &"getChildAt".into())?.dyn_into()?;
    let mut col = |i: usize| -> Result<JsValue, JsValue> {
        get_child_at.call1(table, &JsValue::from_f64(i as f64))
    };
    let t_col = col(t_idx)?;
    let v_col = col(v_idx)?;
    let label_cols: Vec<(JsValue, &str)> = {
        let mut out = Vec::with_capacity(label_indices.len());
        for (i, name) in &label_indices {
            out.push((col(*i)?, name.as_str()));
        }
        out
    };

    let n_rows = Reflect::get(table, &"numRows".into())?
        .as_f64()
        .ok_or_else(|| JsValue::from_str("table.numRows not a number"))?
        as usize;

    // Group rows by the tuple of label values. Build groups in insertion
    // order so output is deterministic.
    let mut groups: Vec<(serde_json::Map<String, serde_json::Value>, Vec<(f64, String)>)> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();

    let get_cell = |vector: &JsValue, row: usize| -> Result<JsValue, JsValue> {
        let get: Function = Reflect::get(vector, &"get".into())?.dyn_into()?;
        get.call1(vector, &JsValue::from_f64(row as f64))
    };
    let cell_to_string = |cell: &JsValue| -> String {
        if cell.is_null() || cell.is_undefined() {
            return "null".to_string();
        }
        if let Some(s) = cell.as_string() {
            return s;
        }
        if let Some(n) = cell.as_f64() {
            return n.to_string();
        }
        // BigInt and other JS values: format via String(...) at JS side
        // would be cleaner; for now, debug-format the JsValue.
        format!("{cell:?}")
    };
    let cell_to_v_string = |cell: &JsValue| -> Option<String> {
        if cell.is_null() || cell.is_undefined() {
            return None;
        }
        if let Some(s) = cell.as_string() {
            return Some(s);
        }
        if let Some(n) = cell.as_f64() {
            return Some(n.to_string());
        }
        Some(format!("{cell:?}"))
    };

    for row in 0..n_rows {
        let v_cell = get_cell(&v_col, row)?;
        let v_str = match cell_to_v_string(&v_cell) {
            Some(s) => s,
            None => continue, // NULL value → drop row (Prometheus gap)
        };
        let t_cell = get_cell(&t_col, row)?;
        let t_secs = t_cell.as_f64().ok_or_else(|| {
            JsValue::from_str(&format!("`t` column row {row} is not a number"))
        })?;

        // Build label map for this row (deterministic field ordering).
        let mut metric = serde_json::Map::with_capacity(label_cols.len());
        for (vector, name) in &label_cols {
            let cell = get_cell(vector, row)?;
            metric.insert(
                (*name).to_string(),
                serde_json::Value::String(cell_to_string(&cell)),
            );
        }

        // Group key: deterministic JSON encoding of the metric map.
        let key = serde_json::to_string(&metric).unwrap_or_default();
        let idx = match group_index.get(&key) {
            Some(&idx) => idx,
            None => {
                let idx = groups.len();
                group_index.insert(key, idx);
                groups.push((metric, Vec::new()));
                idx
            }
        };
        groups[idx].1.push((t_secs, v_str));
    }

    // Emit the Prometheus matrix JSON.
    let mut out = String::new();
    out.push_str("{\"status\":\"success\",\"data\":{\"resultType\":\"matrix\",\"result\":[");
    for (i, (metric, values)) in groups.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"metric\":");
        out.push_str(&serde_json::to_string(metric).unwrap_or_else(|_| "{}".into()));
        out.push_str(",\"values\":[");
        for (j, (t, v)) in values.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&t.to_string());
            out.push_str(",\"");
            // Escape backslashes and quotes (rare for numeric value strings).
            for ch in v.chars() {
                if ch == '\\' || ch == '"' {
                    out.push('\\');
                }
                out.push(ch);
            }
            out.push_str("\"]");
        }
        out.push_str("]}");
    }
    out.push_str("]}}");
    Ok(out)
}

/// Read the (0, 0) cell of an Arrow Table as f64. Matches the convention
/// in the wasm-poc DuckSession.
fn first_double_cell(table: &JsValue) -> Result<f64, JsValue> {
    let get_child: Function = Reflect::get(table, &"getChildAt".into())?.dyn_into()?;
    let vector = get_child.call1(table, &JsValue::from_f64(0.0))?;
    if vector.is_null() || vector.is_undefined() {
        return Err(JsValue::from_str("getChildAt(0) returned null"));
    }
    let get: Function = Reflect::get(&vector, &"get".into())?.dyn_into()?;
    let cell = get.call1(&vector, &JsValue::from_f64(0.0))?;
    cell.as_f64()
        .ok_or_else(|| JsValue::from_str(&format!("cell is not a number: {cell:?}")))
}
