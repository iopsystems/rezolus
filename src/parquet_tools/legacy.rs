//! Recordings in the formats earlier releases wrote (`.rez` archives,
//! both containers, standalone parquet files, `.parquet.ab.tar` A/B
//! tarballs), turned into `.dendro` by `recording upgrade`, and the staging
//! every command that writes an archive shares.
//!
//! No other command opens those formats: each refuses one with the upgrade
//! command (`rez::container::not_dendro`).

use std::path::{Path, PathBuf};

pub(crate) use ::rez::container::Container;

/// The parquet converter for the file at `path`: its source labelled
/// `source=<name>` when the file's `source` metadata names one source and no
/// series carries a `source` label of its own
/// (`dashboard::combined::parquet_source_label`), so a KPI template that
/// selects `{source="<name>"}` matches the converted recording's series.
pub(crate) fn parquet_converter(path: &Path) -> metriken_storage::to_dendro::ParquetToArchive {
    let converter = metriken_storage::to_dendro::ParquetToArchive::new();
    match metriken_query::ParquetReader::open(path)
        .ok()
        .and_then(|r| ::dashboard::combined::parquet_source_label(&r))
    {
        Some(source) => converter.label("source", source),
        None => converter,
    }
}

/// Convert the legacy recording at `input` into a new `.dendro` at
/// `output`, which must not exist, as `upgrade` does: for tests that need
/// an upgraded fixture.
#[cfg(test)]
pub(crate) fn convert(input: &Path, output: &Path) -> Result<String, Box<dyn std::error::Error>> {
    convert_quiet(input, output)
}

/// Convert `input` to `output` and say what was written (sources,
/// segments, rows), without naming either.
pub(crate) fn convert_quiet(
    input: &Path,
    output: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    use metriken_storage::to_dendro::{convert_path, ConvertedFrom};
    let container = Container::of_path(input);
    let summary = match container {
        Container::Parquet if super::combined_parquet::is_combined(input) => {
            super::combined_parquet::convert(input, output)?
        }
        Container::Parquet => {
            let w = parquet_converter(input)
                .convert_file_path(input, output)
                .map_err(|e| format!("{}: {e}", input.display()))?;
            format!(
                "{} source(s), {} segment(s), {} row(s)",
                w.sources, w.segments, w.rows
            )
        }
        Container::RezV3 | Container::RezTar => {
            match convert_path(input, output).map_err(|e| e as Box<dyn std::error::Error>)? {
                ConvertedFrom::RezV3(c) => format!(
                    "{} source(s), {} segment(s), {} live WAL row(s), {} clock offset(s)",
                    c.sources, c.segments, c.wal_rows, c.clock_offsets
                ),
                ConvertedFrom::RezTar(w) | ConvertedFrom::Parquet(w) => format!(
                    "{} source(s), {} segment(s), {} row(s)",
                    w.sources, w.segments, w.rows
                ),
                _ => "converted".to_string(),
            }
        }
        Container::AbTar => super::ab_tarball::convert(input, output)?,
        Container::Dendro => {
            return Err(format!("{} is already a dendro archive", input.display()).into())
        }
        Container::Unknown => {
            return Err(format!(
                "{} is not a recording (a .dendro archive, or a .rez, .parquet or \
                 .parquet.ab.tar to upgrade)",
                input.display()
            )
            .into())
        }
    };
    Ok(summary)
}

/// Whether `a` and `b` name the same file: their canonical paths, or for a
/// path that does not exist, its canonical directory and file name.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| -> Option<PathBuf> {
        if let Ok(c) = p.canonicalize() {
            return Some(c);
        }
        let dir = p
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Some(dir.canonicalize().ok()?.join(p.file_name()?))
    };
    match (canonical(a), canonical(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// Refuse `output` unless it names a new `.dendro`.
pub(crate) fn check_new_dendro(output: &Path) -> Result<(), String> {
    if output.extension().and_then(|e| e.to_str()) != Some("dendro") {
        return Err(format!(
            "{}: the output is a .dendro archive; give a name ending in .dendro",
            output.display()
        ));
    }
    if output.exists() {
        return Err(format!(
            "{} already exists; refusing to replace it",
            output.display()
        ));
    }
    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !dir.is_dir() {
        return Err(format!(
            "{}: the directory {} does not exist",
            output.display(),
            dir.display()
        ));
    }
    Ok(())
}

/// `input` with its recording extension replaced by `.dendro` (`run.rez`,
/// `run.parquet`, `run.parquet.ab.tar`, `run.raw.zst` → `run.dendro`); any
/// other extension is replaced as `Path::with_extension` replaces it.
pub(crate) fn dendro_beside(input: &Path) -> PathBuf {
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for suffix in [
        ".parquet.ab.tar",
        ".ab.tar",
        ".raw.zst",
        ".rez",
        ".parquet",
        ".raw",
    ] {
        if let Some(stem) = name.strip_suffix(suffix).filter(|s| !s.is_empty()) {
            return input.with_file_name(format!("{stem}.dendro"));
        }
    }
    input.with_extension("dendro")
}

/// Where an edit of a recording is written: [`edit_target`]'s result.
///
/// [`path`](Self::path) is the archive to edit. For an edit written to a new
/// `.dendro` it is a staged file in a directory beside the output, and
/// [`commit`](Self::commit) renames it into place once the edit succeeded;
/// dropped without a commit, the staging directory goes and nothing is left
/// at the output.
pub(crate) struct EditTarget {
    path: PathBuf,
    note: Option<String>,
    staged: Option<(tempfile::TempDir, PathBuf)>,
}

impl EditTarget {
    /// The archive to edit.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The path the edit ends up at, for messages: the output, or the input
    /// edited in place.
    pub(crate) fn shown(&self) -> &Path {
        self.staged
            .as_ref()
            .map_or(self.path.as_path(), |(_, output)| output.as_path())
    }

    /// What was converted or copied, when anything was.
    pub(crate) fn note(&self) -> Option<String> {
        self.note.clone()
    }

    /// Move a staged edit to its output ([`publish`]), which must still not
    /// exist.
    pub(crate) fn commit(self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some((dir, output)) = self.staged {
            publish(&self.path, &output)?;
            drop(dir);
        }
        Ok(())
    }
}

/// A staging directory beside `output`, so publishing it stays on one
/// filesystem. It is named `.<output name>.rezolus-staging.<pid>.<random>`,
/// the output name truncated to its first 64 characters so the directory
/// name stays under the 255-byte limit, so one a killed process left behind
/// says what it was; it is removed when the returned `TempDir` drops.
pub(crate) fn staging_dir(output: &Path) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name: String = output
        .file_name()
        .map(|n| n.to_string_lossy().chars().take(64).collect())
        .unwrap_or_default();
    tempfile::Builder::new()
        .prefix(&format!(".{name}.rezolus-staging.{}.", std::process::id()))
        .tempdir_in(dir)
        .map_err(|e| format!("{}: cannot write beside it ({e})", output.display()).into())
}

/// [`staging_dir`] and the staged file in it.
pub(crate) fn stage_beside(
    output: &Path,
) -> Result<(tempfile::TempDir, PathBuf), Box<dyn std::error::Error>> {
    let staging = staging_dir(output)?;
    let staged = staging.path().join("edit.dendro");
    Ok((staging, staged))
}

/// Put the staged file at `output` without replacing anything: a hard link,
/// then the staged name removed, so an `output` that appeared since it was
/// checked is refused rather than replaced. Where links are not supported,
/// the file is copied into an `output` created new (`create_new`), and a
/// copy that fails part way is removed.
pub(crate) fn publish(staged: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match std::fs::hard_link(staged, output) {
        Ok(()) => {
            let _ = std::fs::remove_file(staged);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists(output)),
        Err(_) => copy_new(staged, output),
    }
}

fn exists(output: &Path) -> Box<dyn std::error::Error> {
    format!(
        "{} already exists; refusing to replace it",
        output.display()
    )
    .into()
}

/// [`publish`] where links are not supported: `staged` copied into `output`
/// created new (`create_new`), so an existing file is refused, and a copy
/// that fails part way removed.
fn copy_new(staged: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(exists(output)),
        Err(e) => return Err(format!("{}: {e}", output.display()).into()),
    };
    let copied = std::fs::File::open(staged)
        .and_then(|mut src| std::io::copy(&mut src, &mut out))
        .and_then(|_| out.sync_all());
    if let Err(e) = copied {
        drop(out);
        let _ = std::fs::remove_file(output);
        return Err(format!("{}: {e}", output.display()).into());
    }
    let _ = std::fs::remove_file(staged);
    Ok(())
}

/// Where an edit of the recording at `input` is written.
///
/// A dendro archive is edited in place, or with `output` the edit goes to a
/// copy of it there, staged beside `output` and moved there by
/// [`EditTarget::commit`], so a failed edit leaves nothing at `output`. Any
/// other file is refused; one an earlier release wrote names the `upgrade`
/// command, spelled with `program`.
pub(crate) fn edit_target(
    input: &Path,
    output: Option<&Path>,
    program: &str,
) -> Result<EditTarget, Box<dyn std::error::Error>> {
    match (Container::of_path(input), output) {
        (Container::Dendro, None) => Ok(EditTarget {
            path: input.to_path_buf(),
            note: None,
            staged: None,
        }),
        (Container::Dendro, Some(output)) => {
            check_new_dendro(output)?;
            let (dir, staged) = stage_beside(output)?;
            let src = dendro::archive::Archive::open(input)?;
            crate::dendro_copy::copy(&src, &staged, &dendro::rewrite::CopySpec::everything())?;
            Ok(EditTarget {
                path: staged,
                note: Some(format!(
                    "copied {} to {}",
                    input.display(),
                    output.display()
                )),
                staged: Some((dir, output.to_path_buf())),
            })
        }
        (c, _) => {
            Err(::rez::container::not_dendro(&input.display().to_string(), c, program).into())
        }
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;

    /// Publishing never replaces a file: an output that appeared after it
    /// was checked is refused and kept as it was. The staging directory is
    /// named for its output and this process.
    #[test]
    fn publish_refuses_an_output_that_appeared_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.dendro");
        let (staging, staged) = stage_beside(&output).unwrap();
        let name = staging
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&format!(
                ".out.dendro.rezolus-staging.{}.",
                std::process::id()
            )),
            "{name}"
        );
        std::fs::write(&staged, b"new").unwrap();
        std::fs::write(&output, b"theirs").unwrap();
        let err = publish(&staged, &output).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"theirs");

        std::fs::remove_file(&output).unwrap();
        publish(&staged, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
        assert!(!staged.exists(), "the staged name is removed");
    }

    /// An output whose name is near the 255-byte limit still stages: the
    /// staging directory carries the first 64 characters of it.
    #[test]
    fn a_long_output_name_stages() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("{}.dendro", "x".repeat(233));
        assert_eq!(name.len(), 240);
        let output = dir.path().join(&name);
        let (staging, staged) = stage_beside(&output).unwrap();
        let dir_name = staging
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(dir_name.starts_with(&format!(".{}.rezolus-staging.", "x".repeat(64))));
        std::fs::write(&staged, b"new").unwrap();
        publish(&staged, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
    }

    /// The copy used where links are not supported never replaces a file
    /// either.
    #[test]
    fn the_copy_fallback_refuses_an_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let output = dir.path().join("out.dendro");
        std::fs::write(&staged, b"new").unwrap();
        std::fs::write(&output, b"theirs").unwrap();
        let err = copy_new(&staged, &output).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&output).unwrap(), b"theirs");
        std::fs::remove_file(&output).unwrap();
        copy_new(&staged, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
    }
}
