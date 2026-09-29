//! `export_query`: a PromQL range query materialized as a file the agent can
//! open with whatever it has, the escape from the PromQL round trip.
//!
//! The file is long-form, one row per (series, timestamp): `series` is the
//! label set rendered as the CLI prints it (`{k="v", ...}`), `timestamp` is
//! Unix seconds, `value` the sample, and `lo`/`hi` the acquisition-window
//! uncertainty band when the query carried one (`rate()`/`irate()`), null
//! otherwise. CSV and parquet carry the same columns.
//!
//! Files land only under the directory `rezolus mcp --export-dir` names,
//! under a bare file name, and never over an existing file: the agent
//! chooses a name or gets one derived from the query, and a path with
//! separators or `..` is refused rather than resolved.

use std::path::{Path, PathBuf};

use metriken_query::QueryResult;
use serde::Serialize;

/// One exported row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub series: String,
    pub timestamp: f64,
    pub value: f64,
    pub lo: Option<f64>,
    pub hi: Option<f64>,
}

/// What `export_query` reports back.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Exported {
    pub path: PathBuf,
    pub format: Format,
    pub rows: usize,
    pub series: usize,
    pub columns: [&'static str; 5],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Format {
    Csv,
    Parquet,
}

impl Format {
    pub(crate) fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("csv") => Ok(Format::Csv),
            Some("parquet") => Ok(Format::Parquet),
            Some(other) => Err(format!(
                "format must be \"csv\" or \"parquet\", not {other:?}"
            )),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Parquet => "parquet",
        }
    }
}

pub(crate) const COLUMNS: [&str; 5] = ["series", "timestamp", "value", "lo", "hi"];

/// Flatten a query result into rows. A heatmap has no scalar value per
/// point and is refused: the caller wants `histogram_quantile(...)`.
pub(crate) fn rows_of(result: &QueryResult) -> Result<Vec<Row>, String> {
    let series_name = |m: &std::collections::HashMap<String, String>| {
        if m.is_empty() {
            return String::from("{}");
        }
        let mut parts: Vec<String> = m.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        parts.sort();
        format!("{{{}}}", parts.join(", "))
    };
    match result {
        QueryResult::Matrix { result } => {
            let mut rows = Vec::new();
            for s in result {
                let name = series_name(&s.metric);
                for (i, (t, v)) in s.values.iter().enumerate() {
                    let band = s.bands.as_ref().and_then(|b| b.get(i).copied().flatten());
                    rows.push(Row {
                        series: name.clone(),
                        timestamp: *t,
                        value: *v,
                        lo: band.map(|(lo, _)| lo),
                        hi: band.map(|(_, hi)| hi),
                    });
                }
            }
            Ok(rows)
        }
        QueryResult::Vector { result } => Ok(result
            .iter()
            .map(|s| Row {
                series: series_name(&s.metric),
                timestamp: s.value.0,
                value: s.value.1,
                lo: None,
                hi: None,
            })
            .collect()),
        QueryResult::Scalar { result: (t, v) } => Ok(vec![Row {
            series: String::from("{}"),
            timestamp: *t,
            value: *v,
            lo: None,
            hi: None,
        }]),
        QueryResult::HistogramHeatmap { .. } => Err(
            "the query produced a heatmap, which has no one value per point to export; \
             export a quantile of it instead, e.g. histogram_quantile(0.99, <metric>)"
                .into(),
        ),
    }
}

/// A file name the agent may choose: one path component, no `..`, no
/// separators, no leading dot, and the format's extension is appended when
/// missing.
pub(crate) fn file_name(
    requested: Option<&str>,
    query: &str,
    format: Format,
) -> Result<String, String> {
    let ext = format.extension();
    let name = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => {
            let p = Path::new(n);
            let mut comps = p.components();
            let one = comps.next();
            if comps.next().is_some()
                || !matches!(one, Some(std::path::Component::Normal(_)))
                || n.contains(['/', '\\'])
                || n.starts_with('.')
            {
                return Err(format!(
                    "filename {n:?} must be a bare file name (no directories, no leading dot); \
                     files land under the server's --export-dir"
                ));
            }
            n.to_string()
        }
        None => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(query.as_bytes());
            let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            format!("query-{hex}-{stamp}")
        }
    };
    Ok(if name.ends_with(&format!(".{ext}")) {
        name
    } else {
        format!("{name}.{ext}")
    })
}

/// Write `rows` under `dir` as `name`, refusing to overwrite.
pub(crate) fn write(
    dir: &Path,
    name: &str,
    format: Format,
    rows: &[Row],
) -> Result<Exported, String> {
    if !dir.is_dir() {
        return Err(format!(
            "export directory {} does not exist or is not a directory",
            dir.display()
        ));
    }
    let path = dir.join(name);
    if path.exists() {
        return Err(format!(
            "{} already exists; choose another filename (exports never overwrite)",
            path.display()
        ));
    }
    match format {
        Format::Csv => {
            write_csv(&path, rows).map_err(|e| format!("writing {}: {e}", path.display()))?
        }
        Format::Parquet => {
            write_parquet(&path, rows).map_err(|e| format!("writing {}: {e}", path.display()))?
        }
    }
    let mut names: Vec<&str> = rows.iter().map(|r| r.series.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    Ok(Exported {
        path,
        format,
        rows: rows.len(),
        series: names.len(),
        columns: COLUMNS,
    })
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_csv(path: &Path, rows: &[Row]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "{}", COLUMNS.join(","))?;
    let num = |v: Option<f64>| v.map(|x| format!("{x}")).unwrap_or_default();
    for r in rows {
        writeln!(
            out,
            "{},{},{},{},{}",
            csv_field(&r.series),
            r.timestamp,
            r.value,
            num(r.lo),
            num(r.hi)
        )?;
    }
    out.flush()
}

fn write_parquet(path: &Path, rows: &[Row]) -> Result<(), Box<dyn std::error::Error>> {
    use arrow::array::{ArrayRef, Float64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    let schema = Arc::new(Schema::new(vec![
        Field::new("series", DataType::Utf8, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("value", DataType::Float64, false),
        Field::new("lo", DataType::Float64, true),
        Field::new("hi", DataType::Float64, true),
    ]));
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(
            rows.iter().map(|r| r.series.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.timestamp).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.value).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.lo).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter().map(|r| r.hi).collect::<Vec<_>>(),
        )),
    ];
    let batch = RecordBatch::try_new(schema.clone(), arrays)?;
    let file = std::fs::File::create(path)?;
    let props = WriterProperties::builder().build();
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::{MatrixSample, Sample};
    use std::collections::HashMap;

    fn matrix() -> QueryResult {
        let m = |k: &str, v: &str| HashMap::from([(k.to_string(), v.to_string())]);
        QueryResult::Matrix {
            result: vec![
                MatrixSample::new(m("id", "0"), vec![(1.0, 10.0), (2.0, 11.0)])
                    .with_bands(Some(vec![Some((9.0, 11.0)), None])),
                MatrixSample::new(m("id", "1,x"), vec![(1.0, 20.0)]),
            ],
        }
    }

    #[test]
    fn rows_are_long_form_with_bands_when_present() {
        let rows = rows_of(&matrix()).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].series, "{id=\"0\"}");
        assert_eq!((rows[0].lo, rows[0].hi), (Some(9.0), Some(11.0)));
        assert_eq!((rows[1].lo, rows[1].hi), (None, None));
        assert_eq!(rows[2].series, "{id=\"1,x\"}");
        let v = rows_of(&QueryResult::Vector {
            result: vec![Sample::new(HashMap::new(), (5.0, 1.5))],
        })
        .unwrap();
        assert_eq!(v[0].series, "{}");
        assert_eq!(v[0].timestamp, 5.0);
        let s = rows_of(&QueryResult::Scalar { result: (7.0, 2.0) }).unwrap();
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn file_names_are_bare_and_get_the_extension() {
        assert_eq!(file_name(Some("cpu"), "q", Format::Csv).unwrap(), "cpu.csv");
        assert_eq!(
            file_name(Some("cpu.csv"), "q", Format::Csv).unwrap(),
            "cpu.csv"
        );
        assert_eq!(
            file_name(Some("cpu"), "q", Format::Parquet).unwrap(),
            "cpu.parquet"
        );
        for bad in ["../x", "a/b", "/abs", ".hidden", "a\\b", ""] {
            let r = file_name(Some(bad), "q", Format::Csv);
            if bad.is_empty() {
                assert!(r.unwrap().starts_with("query-"), "empty means derived");
            } else {
                assert!(r.is_err(), "{bad:?} must be refused");
            }
        }
        let derived = file_name(None, "sum(rate(x[1m]))", Format::Csv).unwrap();
        assert!(
            derived.starts_with("query-") && derived.ends_with(".csv"),
            "{derived}"
        );
        assert_eq!(Format::parse(None).unwrap(), Format::Csv);
        assert_eq!(Format::parse(Some("Parquet")).unwrap(), Format::Parquet);
        assert!(Format::parse(Some("json")).is_err());
    }

    #[test]
    fn csv_and_parquet_carry_the_same_columns_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let rows = rows_of(&matrix()).unwrap();
        let csv = write(dir.path(), "out.csv", Format::Csv, &rows).unwrap();
        assert_eq!((csv.rows, csv.series), (3, 2));
        let text = std::fs::read_to_string(&csv.path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "series,timestamp,value,lo,hi");
        assert_eq!(lines[1], "\"{id=\"\"0\"\"}\",1,10,9,11");
        assert_eq!(lines[2], "\"{id=\"\"0\"\"}\",2,11,,");
        assert!(write(dir.path(), "out.csv", Format::Csv, &rows)
            .err()
            .unwrap()
            .contains("already exists"));

        let pq = write(dir.path(), "out.parquet", Format::Parquet, &rows).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            std::fs::File::open(&pq.path).unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        let batches: Vec<_> = reader.map(|b| b.unwrap()).collect();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 3);
        let names: Vec<String> = batches[0]
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, COLUMNS);
        assert!(
            write(Path::new("/nonexistent/dir"), "x.csv", Format::Csv, &rows)
                .err()
                .unwrap()
                .contains("does not exist")
        );
    }
}
