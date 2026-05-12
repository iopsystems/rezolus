//! Extract a combined-A/B tar archive (produced by `parquet combine --ab`)
//! into two per-side parquets on disk so the viewer can load each with
//! `Tsdb::load`.
//!
//! Wire format (see `src/parquet_tools/combine.rs::write_ab_tarball`):
//! a POSIX/USTAR-style tar containing exactly three entries —
//! `baseline.parquet`, `experiment.parquet`, and `ab.json` (an
//! `AbContainers` manifest). Order is not enforced. The two parquets
//! inside are unmodified bytes of the original captures, so each is
//! independently valid on its own.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tempfile::TempDir;

use crate::parquet_tools::combine::{AB_BASELINE_NAME, AB_EXPERIMENT_NAME, AB_MANIFEST_NAME};

/// Result of extracting a combined-A/B tar. The tempdir is held to keep
/// both parquet paths alive — drop deletes everything.
pub struct ExtractedAb {
    pub manifest: crate::parquet_metadata::AbContainers,
    /// The parquet files are written under this tempdir; dropping the
    /// struct deletes them.
    _dir: TempDir,
    pub baseline_path: std::path::PathBuf,
    pub experiment_path: std::path::PathBuf,
}

/// Cheap probe: does this file *look* like a tar archive? Checks the
/// POSIX-tar magic at offset 257 ("ustar") and, as a safety net, that
/// the file does not end with parquet's "PAR1" footer. Used at viewer
/// load time to decide whether to dispatch to bare-parquet or AB-tar
/// loading.
pub fn looks_like_ab_tarball(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return false;
    };
    // Parquet footers end with the four magic bytes "PAR1"; if we see
    // them, this is definitely a parquet file, not a tar.
    if len >= 4 {
        let mut tail = [0u8; 4];
        if f.seek(SeekFrom::End(-4)).is_ok() && f.read_exact(&mut tail).is_ok() && &tail == b"PAR1"
        {
            return false;
        }
    }
    // USTAR magic at offset 257 in the first 512-byte header block.
    if f.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    let mut header = [0u8; 512];
    if f.read_exact(&mut header).is_err() {
        return false;
    }
    &header[257..262] == b"ustar"
}

pub fn extract_ab_tarball(path: &Path) -> Result<ExtractedAb, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    let dir = tempfile::Builder::new().prefix("rezolus-ab-").tempdir()?;

    let mut baseline_path = None;
    let mut experiment_path = None;
    let mut manifest_bytes: Option<Vec<u8>> = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.to_path_buf();
        let Some(name) = entry_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        match name {
            AB_BASELINE_NAME => {
                let dest = dir.path().join(AB_BASELINE_NAME);
                entry.unpack(&dest)?;
                baseline_path = Some(dest);
            }
            AB_EXPERIMENT_NAME => {
                let dest = dir.path().join(AB_EXPERIMENT_NAME);
                entry.unpack(&dest)?;
                experiment_path = Some(dest);
            }
            AB_MANIFEST_NAME => {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                manifest_bytes = Some(buf);
            }
            _ => {
                // Tolerate unknown entries (forward-compat). The combine
                // writer only emits three names today.
            }
        }
    }

    let baseline_path = baseline_path.ok_or("combined-A/B tarball missing baseline.parquet")?;
    let experiment_path =
        experiment_path.ok_or("combined-A/B tarball missing experiment.parquet")?;
    let manifest_bytes = manifest_bytes.ok_or("combined-A/B tarball missing ab.json manifest")?;
    let manifest: crate::parquet_metadata::AbContainers = serde_json::from_slice(&manifest_bytes)?;

    if manifest.version != crate::parquet_metadata::AbContainers::SCHEMA_VERSION {
        return Err(format!(
            "combined-A/B tarball has manifest schema version {} (expected {})",
            manifest.version,
            crate::parquet_metadata::AbContainers::SCHEMA_VERSION
        )
        .into());
    }

    Ok(ExtractedAb {
        manifest,
        _dir: dir,
        baseline_path,
        experiment_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parquet_metadata::{AbContainers, AbSide};

    /// Round-trip: pack two single-source parquets via the combine writer,
    /// then extract them via this module and confirm both come back byte-
    /// identical with a matching manifest.
    #[test]
    fn extract_round_trip() {
        // Use the existing combine test helpers to materialize two minimal
        // parquets on disk, package them with `parquet combine --ab`, then
        // extract.
        use arrow::array::{Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use parquet::file::metadata::KeyValue;
        use parquet::file::properties::WriterProperties;
        use std::sync::Arc;

        fn write_test_parquet(path: &Path, source: &str, base_ts: u64) {
            let schema = Arc::new(Schema::new(vec![
                Field::new("timestamp", DataType::UInt64, false),
                Field::new("duration", DataType::UInt64, false),
                Field::new("queue_depth", DataType::Int64, false).with_metadata(
                    [("metric_type".to_string(), "gauge".to_string())]
                        .into_iter()
                        .collect(),
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
                KeyValue {
                    key: "source".to_string(),
                    value: Some(source.to_string()),
                },
                KeyValue {
                    key: "sampling_interval_ms".to_string(),
                    value: Some("1000".to_string()),
                },
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

        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.parquet");
        let b = dir.path().join("b.parquet");
        write_test_parquet(&a, "vllm", 1_000_000_000);
        write_test_parquet(&b, "sglang", 10_000_000_000);

        // Build a fake tar through the combine writer would couple this
        // test to the CLI. Instead, hand-craft a tar that matches the
        // writer's shape and confirm we extract correctly.
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
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();

        let tar_path = dir.path().join("combined.parquet.ab.tar");
        {
            let f = std::fs::File::create(&tar_path).unwrap();
            let mut builder = tar::Builder::new(f);
            builder.mode(tar::HeaderMode::Deterministic);
            for (name, src) in [(AB_BASELINE_NAME, &a), (AB_EXPERIMENT_NAME, &b)] {
                let mut sf = std::fs::File::open(src).unwrap();
                let len = sf.metadata().unwrap().len();
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(len);
                header.set_mode(0o644);
                header.set_cksum();
                let mut buf = Vec::new();
                sf.read_to_end(&mut buf).unwrap();
                builder.append(&header, &buf[..]).unwrap();
            }
            let mut header = tar::Header::new_gnu();
            header.set_path(AB_MANIFEST_NAME).unwrap();
            header.set_size(manifest_bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &manifest_bytes[..]).unwrap();
            builder.into_inner().unwrap().sync_all().unwrap();
        }

        assert!(looks_like_ab_tarball(&tar_path));
        assert!(!looks_like_ab_tarball(&a));

        let extracted = extract_ab_tarball(&tar_path).expect("extract must succeed");
        assert_eq!(extracted.manifest.baseline.alias, "vllm");
        assert_eq!(extracted.manifest.experiment.alias, "sglang");

        let orig_a = std::fs::read(&a).unwrap();
        let orig_b = std::fs::read(&b).unwrap();
        let new_a = std::fs::read(&extracted.baseline_path).unwrap();
        let new_b = std::fs::read(&extracted.experiment_path).unwrap();
        assert_eq!(orig_a, new_a, "baseline bytes survive tar round-trip");
        assert_eq!(orig_b, new_b, "experiment bytes survive tar round-trip");
    }
}
