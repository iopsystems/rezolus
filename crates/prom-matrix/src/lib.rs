//! Arrow → Prometheus matrix-shape JSON.
//!
//! Two entry points, one shape:
//!
//! - `arrow_to_prom_matrix(&[RecordBatch]) -> String` — native, used by the
//!   server-backed viewer.
//! - `js_arrow_to_prom_matrix(&JsValue) -> Result<String, JsValue>` — WASM,
//!   walks a JS Arrow Table from duckdb-wasm. Used by the static viewer.
//!
//! Output shape (matches Prometheus `query_range`):
//! ```json
//! {"status":"success","data":{"resultType":"matrix","result":[
//!   {"metric": {"<label>":"<value>", ...}, "values": [[t_seconds, "v_string"], ...]},
//!   ...
//! ]}}
//! ```
//!
//! Column-role detection from the schema:
//!   - field named `t` → timestamp axis (DOUBLE seconds since epoch).
//!   - field named `v` → numeric value axis. NULL rows are dropped
//!     (Prometheus series gap semantics).
//!   - all other fields → labels keying rows into series. Stringified for
//!     the metric label dictionary.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::arrow_to_prom_matrix;

mod wasm;
pub use wasm::js_arrow_to_prom_matrix;

/// JSON for an empty Prometheus matrix result. Returned when the query has
/// no matching rows so the frontend can render the section as empty without
/// special-casing missing data.
pub const EMPTY_PROM_MATRIX: &str =
    r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;

/// One Prometheus matrix series: a label dictionary plus an ordered list of
/// `(t_seconds, v_stringified)` samples. The label map is the `metric` JSON
/// object; samples ship as strings to match Prometheus' wire shape.
pub(crate) type SeriesGroup = (
    serde_json::Map<String, serde_json::Value>,
    Vec<(f64, String)>,
);

/// Emit the Prometheus matrix JSON envelope around a series-grouped
/// projection. Shared by the native and WASM entry points so they cannot
/// drift on escaping or ordering.
pub(crate) fn emit_prom_matrix_json(groups: &[SeriesGroup]) -> String {
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
    out
}
