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
//! separators or `..` is refused rather than resolved. The file is opened
//! with `create_new`, so a symlink planted in the directory (dangling or
//! not) cannot redirect the write elsewhere, and there is no window between
//! the existence check and the create.
//!
//! An export is bounded by [`MAX_ROWS`]: past it the call is refused with
//! the count and a hint to raise `step`, since a run that materializes
//! millions of rows in a process the client cannot see would end as an OOM
//! kill with nothing telling the agent why.

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

/// The most rows one export may hold. Measured at roughly 730 bytes per row
/// in flight for the parquet path (debug build), so this is on the order of
/// 700 MB at the cap, and a 64-CPU day at a 1 s step (5.5 M rows) is
/// refused rather than attempted.
pub(crate) const MAX_ROWS: usize = 1_000_000;

/// How many rows `rows_of` would produce, before producing any.
pub(crate) fn row_count(result: &QueryResult) -> usize {
    match result {
        QueryResult::Matrix { result } => result.iter().map(|s| s.values.len()).sum(),
        QueryResult::Vector { result } => result.len(),
        QueryResult::Scalar { .. } => 1,
        QueryResult::HistogramHeatmap { .. } => 0,
    }
}

/// Refuse a result over the cap, naming the count and the lever.
pub(crate) fn check_row_cap(result: &QueryResult, step: f64) -> Result<(), String> {
    let n = row_count(result);
    if n > MAX_ROWS {
        return Err(format!(
            "the query produces {n} rows at step {step} s, over the export cap of {MAX_ROWS}; \
             raise `step`, aggregate the series (sum(), avg()), or add label matchers"
        ));
    }
    Ok(())
}

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
            // Millisecond resolution: two exports of one query in the same
            // second must not collide on the derived name.
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ");
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
    // `create_new`: refuses an existing file AND a symlink at this name,
    // dangling or not (O_EXCL does not follow it), so nothing in the
    // directory can redirect the write outside it.
    let file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "{} already exists; choose another filename (exports never overwrite)",
                path.display()
            ));
        }
        Err(e) => return Err(format!("creating {}: {e}", path.display())),
    };
    let written = match format {
        Format::Csv => {
            write_csv(file, rows).map_err(|e| format!("writing {}: {e}", path.display()))
        }
        Format::Parquet => {
            write_parquet(file, rows).map_err(|e| format!("writing {}: {e}", path.display()))
        }
    };
    if let Err(e) = written {
        // A half-written file under a name the agent may retry with.
        let _ = std::fs::remove_file(&path);
        return Err(e);
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

fn write_csv(file: std::fs::File, rows: &[Row]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(file);
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

fn write_parquet(file: std::fs::File, rows: &[Row]) -> Result<(), Box<dyn std::error::Error>> {
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

    /// A dangling symlink planted under the export dir must not become a
    /// write outside it: `exists()` says false for it, `File::create` would
    /// follow it. `create_new` refuses it.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_in_the_export_dir_cannot_redirect_the_write() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("victim.csv");
        std::os::unix::fs::symlink(&victim, dir.path().join("dangle.csv")).unwrap();
        let rows = rows_of(&matrix()).unwrap();
        let err = write(dir.path(), "dangle.csv", Format::Csv, &rows)
            .err()
            .unwrap();
        assert!(err.contains("already exists"), "{err}");
        assert!(!victim.exists(), "nothing may land at the link's target");
        // The same for parquet.
        std::os::unix::fs::symlink(&victim, dir.path().join("dangle.parquet")).unwrap();
        assert!(write(dir.path(), "dangle.parquet", Format::Parquet, &rows).is_err());
        assert!(!victim.exists());
    }

    #[test]
    fn the_row_cap_is_checked_before_any_row_is_built() {
        assert_eq!(row_count(&matrix()), 3);
        assert!(check_row_cap(&matrix(), 1.0).is_ok());
        let m = |k: &str, v: &str| HashMap::from([(k.to_string(), v.to_string())]);
        let big = QueryResult::Matrix {
            result: vec![MatrixSample::new(
                m("id", "0"),
                vec![(0.0, 0.0); MAX_ROWS + 1],
            )],
        };
        let err = check_row_cap(&big, 0.01).err().unwrap();
        assert!(
            err.contains("1000001 rows")
                && err.contains("step 0.01")
                && err.contains("raise `step`"),
            "{err}"
        );
        let d1 = file_name(None, "q", Format::Csv).unwrap();
        assert!(
            d1.len() > "query-xxxxxxxx-20260929T000000000Z.csv".len() - 4,
            "{d1}"
        );
    }
}
