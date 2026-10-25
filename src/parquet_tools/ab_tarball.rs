//! `.parquet.ab.tar` A/B tarballs, which 5.x `recording combine --ab` wrote,
//! converted by `recording upgrade` into one two-recording `.dendro`: each
//! parquet becomes a source labelled `arm=baseline` or `arm=experiment`,
//! which the viewer opens as a baseline/experiment comparison.
//!
//! Wire format: a tar holding `baseline.parquet`, `experiment.parquet` and
//! `ab.json` (an [`AbContainers`] manifest), in any order. The two parquet
//! files are unmodified captures.

use std::io::Read;
use std::path::Path;

/// The entry names of an A/B tarball.
const AB_MANIFEST_NAME: &str = "ab.json";
const AB_BASELINE_NAME: &str = "baseline.parquet";
const AB_EXPERIMENT_NAME: &str = "experiment.parquet";

/// The `ab.json` manifest. Versioned; [`AbContainers::SCHEMA_VERSION`] is
/// the one version 5.x wrote.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AbContainers {
    pub(crate) version: u32,
    pub(crate) baseline: AbSide,
    pub(crate) experiment: AbSide,
    /// A category template the 5.x viewer applied when it opened the
    /// tarball. The converted archive does not carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AbSide {
    /// The side's display name in the 5.x viewer (e.g. "vllm").
    pub(crate) alias: String,
    /// The source names this side's parquet holds.
    pub(crate) sources: Vec<String>,
}

impl AbContainers {
    pub(crate) const SCHEMA_VERSION: u32 = 1;
}

/// Convert the tarball at `input` into a new two-recording dendro archive
/// at `output`, which must not exist. Returns what was written.
pub(crate) fn convert(input: &Path, output: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let unpacked = tempfile::Builder::new()
        .prefix(".ab-tarball.")
        .tempdir_in(dir)?;
    let manifest = unpack(input, unpacked.path())?;

    let mut out = dendro::archive::ArchiveMut::create(output)?;
    let mut rows = 0u64;
    let mut segments = 0u64;
    for (arm, name) in [
        ("baseline", AB_BASELINE_NAME),
        ("experiment", AB_EXPERIMENT_NAME),
    ] {
        let path = unpacked.path().join(name);
        let written = super::legacy::parquet_converter(&path)
            .label(::dashboard::combined::ARM_LABEL, arm)
            .convert_into(std::fs::File::open(&path)?, &mut out)
            .map_err(|e| format!("{}: {name}: {e}", input.display()))?;
        rows += written.rows;
        segments += written.segments as u64;
    }
    drop(out);
    Ok(format!(
        "2 source(s) (arm=baseline: {}, arm=experiment: {}), {segments} segment(s), {rows} row(s)",
        manifest.baseline.alias, manifest.experiment.alias
    ))
}

/// Unpack the tarball at `path` into `dir` and read its manifest.
fn unpack(path: &Path, dir: &Path) -> Result<AbContainers, Box<dyn std::error::Error>> {
    let mut archive = tar::Archive::new(std::fs::File::open(path)?);
    let (mut baseline, mut experiment, mut manifest) = (false, false, None);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.to_path_buf();
        let Some(name) = entry_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        match name {
            AB_BASELINE_NAME => {
                entry.unpack(dir.join(AB_BASELINE_NAME))?;
                baseline = true;
            }
            AB_EXPERIMENT_NAME => {
                entry.unpack(dir.join(AB_EXPERIMENT_NAME))?;
                experiment = true;
            }
            AB_MANIFEST_NAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                manifest = Some(buf);
            }
            _ => {}
        }
    }
    if !baseline {
        return Err(format!("{}: the tarball holds no baseline.parquet", path.display()).into());
    }
    if !experiment {
        return Err(format!(
            "{}: the tarball holds no experiment.parquet",
            path.display()
        )
        .into());
    }
    let manifest =
        manifest.ok_or_else(|| format!("{}: the tarball holds no ab.json", path.display()))?;
    let manifest: AbContainers = serde_json::from_slice(&manifest)
        .map_err(|e| format!("{}: ab.json is not valid: {e}", path.display()))?;
    if manifest.version != AbContainers::SCHEMA_VERSION {
        return Err(format!(
            "{}: ab.json is schema version {} (this release reads {})",
            path.display(),
            manifest.version,
            AbContainers::SCHEMA_VERSION
        )
        .into());
    }
    Ok(manifest)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A parquet file of three `queue_depth` rows labelled `source`.
    fn write_test_parquet(path: &Path, source: &str, base_ts: u64) {
        use arrow::array::{Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use parquet::file::metadata::KeyValue;
        use parquet::file::properties::WriterProperties;
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::UInt64, false),
            Field::new("duration", DataType::UInt64, false),
            Field::new("queue_depth", DataType::Int64, false).with_metadata(
                [("metric_type".to_string(), "gauge".to_string())]
                    .into_iter()
                    .collect::<std::collections::HashMap<_, _>>(),
            ),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![
                    base_ts,
                    base_ts + 1_000_000_000,
                    base_ts + 2_000_000_000,
                ])),
                Arc::new(UInt64Array::from(vec![1_000_000_000u64; 3])),
                Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            ],
        )
        .unwrap();
        let kv = vec![
            KeyValue::new("source".to_string(), source.to_string()),
            KeyValue::new("sampling_interval_ms".to_string(), "1000".to_string()),
        ];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let mut writer =
            ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, Some(props))
                .unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    /// A tarball in the shape 5.x wrote, its sides labelled `vllm` and
    /// `sglang`.
    pub(crate) fn ab_tarball(dir: &Path) -> std::path::PathBuf {
        let a = dir.join("a.parquet");
        let b = dir.join("b.parquet");
        write_test_parquet(&a, "vllm", 1_700_000_000_000_000_000);
        write_test_parquet(&b, "sglang", 1_700_000_010_000_000_000);
        let manifest = AbContainers {
            version: AbContainers::SCHEMA_VERSION,
            baseline: AbSide {
                alias: "vllm".into(),
                sources: vec!["vllm".into()],
            },
            experiment: AbSide {
                alias: "sglang".into(),
                sources: vec!["sglang".into()],
            },
            category: None,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let tar_path = dir.join("combined.parquet.ab.tar");
        let f = std::fs::File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(f);
        builder.mode(tar::HeaderMode::Deterministic);
        let mut append = |name: &str, bytes: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, bytes).unwrap();
        };
        append(AB_BASELINE_NAME, &std::fs::read(&a).unwrap());
        append(AB_EXPERIMENT_NAME, &std::fs::read(&b).unwrap());
        append(AB_MANIFEST_NAME, &manifest_bytes);
        builder.into_inner().unwrap().sync_all().unwrap();
        std::fs::remove_file(a).unwrap();
        std::fs::remove_file(b).unwrap();
        tar_path
    }

    /// A tarball converts to two recordings, one per arm, each labelled with
    /// its parquet's source and answering its own rows.
    #[test]
    fn a_tarball_becomes_a_two_arm_archive() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let tar = ab_tarball(dir.path());
        assert_eq!(
            super::super::legacy::Container::of_path(&tar),
            super::super::legacy::Container::AbTar
        );
        let out = dir.path().join("ab.dendro");
        let summary = convert(&tar, &out).unwrap();
        assert!(summary.contains("arm=baseline: vllm"), "{summary}");
        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(16 << 20),
        )
        .unwrap();
        let labels: Vec<_> = readers.iter().map(|(l, _)| l.clone()).collect();
        assert_eq!(labels.len(), 2, "{labels:?}");
        for (l, (arm, source)) in labels
            .iter()
            .zip([("baseline", "vllm"), ("experiment", "sglang")])
        {
            assert_eq!(l.get("arm").map(String::as_str), Some(arm), "{l:?}");
            assert_eq!(l.get("source").map(String::as_str), Some(source), "{l:?}");
        }
        for (_, r) in &readers {
            let (start, end) = r.time_range().unwrap();
            let got = serde_json::to_value(r.query_range("queue_depth", start, end, 1.0).unwrap())
                .unwrap();
            assert_eq!(got["result"].as_array().map(Vec::len), Some(1), "{got}");
        }
    }

    /// A tarball without its manifest is refused and leaves no output.
    #[test]
    fn a_tarball_missing_a_member_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("broken.parquet.ab.tar");
        let mut builder = tar::Builder::new(std::fs::File::create(&tar_path).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_path(AB_MANIFEST_NAME).unwrap();
        header.set_size(2);
        header.set_cksum();
        builder.append(&header, &b"{}"[..]).unwrap();
        builder.into_inner().unwrap();
        let out = dir.path().join("out.dendro");
        let err = convert(&tar_path, &out).unwrap_err().to_string();
        assert!(err.contains("no baseline.parquet"), "{err}");
    }
}
