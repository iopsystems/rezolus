//! A recording an earlier release wrote, converted in memory into a dendro
//! archive's bytes when a viewer loads it.
//!
//! The viewers convert a `.rez` (both containers) and a parquet recording of
//! one source as `recording upgrade` converts them, with metriken-storage's
//! converters, which touch no file and spawn no thread, so the browser runs
//! them too. Two formats are not converted here and are refused with the
//! upgrade command: a combined parquet file (a 5.x `recording combine`
//! output, whose converter is the binary's) and a `.parquet.ab.tar`.

use crate::container::Container;
use dendro::archive::ArchiveMut;

/// A dendro archive converted from a recording an earlier release wrote.
pub struct Converted {
    /// The archive's bytes.
    pub bytes: Vec<u8>,
    /// What it was converted from.
    pub from: Container,
}

/// Whether a viewer converts `container` on load (a `.rez`, or a parquet
/// recording that [`is_combined_parquet`] does not claim).
pub fn converts(container: Container) -> bool {
    matches!(
        container,
        Container::RezV3 | Container::RezTar | Container::Parquet
    )
}

/// Convert `data`, a `.rez` or a parquet recording of one source, into a
/// dendro archive's bytes, as `recording upgrade` converts it. A parquet
/// file's source is labelled `source=<name>` when the file names one source
/// and no series carries a `source` label of its own
/// (`dashboard::combined::parquet_source_label`).
///
/// Anything else is refused, naming `filename`: a dendro archive, a combined
/// parquet file and a `.parquet.ab.tar` with the `recording upgrade` command
/// spelled with `program`, any other file as not a recording. A conversion
/// that fails gives the converter's error.
///
/// `data` is taken by value: a v3 `.rez` is copied into SQLite and released
/// before the conversion starts, and a parquet file is read in place.
pub fn to_dendro_bytes(data: Vec<u8>, filename: &str, program: &str) -> Result<Converted, String> {
    let container = Container::of_bytes(&data);
    let failed = |e: &dyn std::fmt::Display| format!("failed to convert {filename}: {e}");
    let mut out = ArchiveMut::create_in_memory().map_err(|e| failed(&e))?;
    match container {
        Container::RezV3 => {
            let db = crate::rez_sqlite::RezDb::open_bytes(data).map_err(|e| failed(&e))?;
            crate::to_dendro::convert_v3_into(&db, &mut out).map_err(|e| failed(&e))?;
            // The SQLite image is released before the archive is serialized.
            drop(db);
        }
        Container::RezTar => {
            crate::to_dendro::convert_tar_into(
                std::io::Cursor::new(&data),
                &mut out,
                &metriken_storage::WriterConfig::default(),
            )
            .map_err(|e| failed(&e))?;
        }
        Container::Parquet => {
            let bytes = bytes::Bytes::from(data);
            if is_combined_parquet(bytes.clone()) {
                return Err(needs_upgrade(
                    filename,
                    "combined parquet recording",
                    program,
                ));
            }
            let mut converter = metriken_storage::to_dendro::ParquetToArchive::new();
            if let Some(source) = metriken_query::ParquetReader::open_bytes(bytes.clone())
                .ok()
                .and_then(|r| dashboard::combined::parquet_source_label(&r))
            {
                converter = converter.label("source", source);
            }
            converter
                .convert_into(bytes, &mut out)
                .map_err(|e| failed(&e))?;
        }
        Container::AbTar => return Err(needs_upgrade(filename, container.name(), program)),
        Container::Dendro => return Err(format!("{filename} is already a dendro archive")),
        Container::Unknown => {
            return Err(format!(
                "{filename} is not a recording (a .dendro archive, a .rez or a .parquet)"
            ))
        }
    }
    let bytes = out.serialize().map_err(|e| failed(&e))?;
    Ok(Converted {
        bytes,
        from: container,
    })
}

/// The refusal of a recording a viewer does not convert on load: the
/// `recording upgrade` command that does, spelled with `program`.
fn needs_upgrade(filename: &str, what: &str, program: &str) -> String {
    format!(
        "{filename} is a {what}, which the viewer does not convert when it opens a file. \
         Convert it with:\n  {program} recording upgrade {}\nand open the .dendro it writes.",
        crate::container::shell_word(filename)
    )
}

/// Whether the parquet file `reader` reads is a combined file: its `source`
/// is a JSON array of source names and every metric column carries a
/// `source` label. A file the 5.x recorder (5.21 and later) wrote for one
/// endpoint given an array `source` has unlabelled columns and is one
/// source.
pub fn is_combined_parquet<R: parquet::file::reader::ChunkReader + 'static>(reader: R) -> bool {
    use dashboard::keys::KEY_SOURCE;
    let Ok(builder) =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(reader)
    else {
        return false;
    };
    let array_source = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .and_then(|kv| kv.iter().find(|e| e.key == KEY_SOURCE))
        .and_then(|e| e.value.as_deref())
        .is_some_and(|v| serde_json::from_str::<Vec<String>>(v).is_ok());
    let mut metrics = builder
        .schema()
        .fields()
        .iter()
        .filter(|f| !matches!(f.name().as_str(), "timestamp" | "duration"))
        .peekable();
    array_source
        && metrics.peek().is_some()
        && metrics.all(|f| f.metadata().get(KEY_SOURCE).is_some_and(|s| !s.is_empty()))
}
