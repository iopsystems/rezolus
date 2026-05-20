use std::collections::HashMap;

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};

/// Walk an Arrow JS Table and emit Prometheus matrix-shape JSON. See the
/// crate docs for the column-role contract and output shape.
pub fn js_arrow_to_prom_matrix(table: &JsValue) -> Result<String, JsValue> {
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
    let col = |i: usize| -> Result<JsValue, JsValue> {
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
    let mut groups: Vec<crate::SeriesGroup> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();

    let get_cell = |vector: &JsValue, row: usize| -> Result<JsValue, JsValue> {
        let get: Function = Reflect::get(vector, &"get".into())?.dyn_into()?;
        get.call1(vector, &JsValue::from_f64(row as f64))
    };
    // Convert any JS cell value to a decimal string via JS's `String(x)`
    // — this handles BigInt (UBIGINT/BIGINT columns) which `as_f64` and
    // `as_string` both reject. f64 fast-path avoids the JS round-trip
    // for the common DOUBLE case.
    let js_string: Function = Reflect::get(&js_sys::global(), &"String".into())?.dyn_into()?;
    let to_string_via_js = |cell: &JsValue| -> Result<String, JsValue> {
        if let Some(s) = cell.as_string() {
            return Ok(s);
        }
        if let Some(n) = cell.as_f64() {
            return Ok(n.to_string());
        }
        let s = js_string.call1(&JsValue::NULL, cell)?;
        Ok(s.as_string().unwrap_or_else(|| format!("{cell:?}")))
    };
    let cell_to_string = |cell: &JsValue| -> Result<String, JsValue> {
        if cell.is_null() || cell.is_undefined() {
            return Ok("null".to_string());
        }
        to_string_via_js(cell)
    };
    // Non-finite floats (NaN, ±Inf) become gaps too. `f64::to_string()`
    // produces `"NaN"` / `"inf"` / `"-inf"` which break frontend
    // `Number(...)` parsing; treating them as NULL matches Prometheus
    // semantics and keeps plots clean. Mirrors the native
    // `cell_to_string_opt`'s `is_finite()` guard.
    let cell_to_v_string = |cell: &JsValue| -> Result<Option<String>, JsValue> {
        if cell.is_null() || cell.is_undefined() {
            return Ok(None);
        }
        if matches!(cell.as_f64(), Some(n) if !n.is_finite()) {
            return Ok(None);
        }
        Ok(Some(to_string_via_js(cell)?))
    };

    for row in 0..n_rows {
        let v_cell = get_cell(&v_col, row)?;
        let v_str = match cell_to_v_string(&v_cell)? {
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
                serde_json::Value::String(cell_to_string(&cell)?),
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

    Ok(crate::emit_prom_matrix_json(&groups))
}
