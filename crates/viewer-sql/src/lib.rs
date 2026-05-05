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
    /// Counter metric name → number of distinct label sets.
    pub counters: HashMap<String, usize>,
    pub gauges: HashMap<String, usize>,
    pub histograms: HashMap<String, usize>,
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
