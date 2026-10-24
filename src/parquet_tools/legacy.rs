//! Recordings in the formats earlier releases wrote (`.rez` archives, both
//! containers, and standalone parquet files), turned into `.dendro`.
//!
//! 6.x writes `.dendro` only. Every format stays readable, and a command
//! that changes a recording (`annotate`, `filter`, `combine`, `snapshot`,
//! `check --annotate`, the MCP event tools) writes the result to a new
//! `.dendro` when its input is in an older format, converted here through
//! metriken-storage's converters (`to_dendro`).

use std::path::{Path, PathBuf};

/// What a recording file is, by content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Container {
    Dendro,
    /// A v3 (SQLite) `.rez`.
    RezV3,
    /// A v1 or v2 (tar) `.rez`.
    RezTar,
    Parquet,
    /// Anything else, or a file that could not be read.
    Unknown,
}

impl Container {
    /// The container of the file at `path`.
    pub(crate) fn of(path: &Path) -> Self {
        use metriken_storage::rez::rez::{detect_rez_format, RezFormat};
        if metriken_storage::DendroCatalog::is_archive(path).unwrap_or(false) {
            return Self::Dendro;
        }
        match detect_rez_format(path) {
            Ok(RezFormat::V3Sqlite) => return Self::RezV3,
            Ok(RezFormat::V2Tar) => return Self::RezTar,
            _ => {}
        }
        let mut magic = [0u8; 4];
        let parquet = std::fs::File::open(path)
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
            .is_ok()
            && &magic == b"PAR1";
        if parquet {
            Self::Parquet
        } else {
            Self::Unknown
        }
    }

    /// Whether this is a format 6.x reads but does not write.
    pub(crate) fn is_legacy(self) -> bool {
        matches!(self, Self::RezV3 | Self::RezTar | Self::Parquet)
    }

    /// The format's name in a message.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Dendro => "dendro archive",
            Self::RezV3 => "v3 .rez archive",
            Self::RezTar => "v1/v2 (tar) .rez archive",
            Self::Parquet => "parquet recording",
            Self::Unknown => "file",
        }
    }
}

/// The parquet converter for the file at `path`: its source labelled
/// `source=<name>` when the file's `source` metadata names one source and no
/// series carries a `source` label of its own, as the viewer labels the file
/// when it reads it (`dashboard::combined::label_parquet_by_source`), so the
/// converted archive presents as the file did.
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
/// `output`, which must not exist. Returns a one-line summary of what was
/// written.
pub(crate) fn convert(input: &Path, output: &Path) -> Result<String, Box<dyn std::error::Error>> {
    convert_for(input, output, output)
}

/// [`convert`] into `output`, a staging file the caller turns into `shown`:
/// the summary names `shown`, the path the user asked for.
pub(crate) fn convert_for(
    input: &Path,
    output: &Path,
    shown: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let summary = convert_quiet(input, output)?;
    Ok(format!(
        "converted {} ({}) {} {}: {summary}",
        input.display(),
        Container::of(input).name(),
        if output == shown { "to" } else { "for" },
        shown.display()
    ))
}

/// Convert `input` to `output` and say what was written (sources,
/// segments, rows), without naming either.
pub(crate) fn convert_quiet(
    input: &Path,
    output: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    use metriken_storage::to_dendro::{convert_path, ConvertedFrom};
    let container = Container::of(input);
    let summary = match container {
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
        Container::Dendro => {
            return Err(format!("{} is already a dendro archive", input.display()).into())
        }
        Container::Unknown => {
            return Err(format!(
                "{} is not a recording (.dendro, .rez or .parquet)",
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
            "{}: 6.x writes .dendro archives only; give an output ending in .dendro",
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

/// The command line this process was started with, `-o <output>` added: the
/// command to run for an edit of a legacy recording, which must be written
/// to a new archive.
pub(crate) fn command_with_output(output: &Path) -> String {
    let mut words: Vec<String> = std::env::args().collect();
    if let Some(first) = words.first_mut() {
        if let Some(name) = Path::new(first.as_str()).file_name() {
            *first = name.to_string_lossy().into_owned();
        }
    }
    words.push("-o".to_string());
    words.push(output.display().to_string());
    words
        .iter()
        .map(|w| crate::parquet_tools::select::shell_word(w))
        .collect::<Vec<_>>()
        .join(" ")
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
/// copy of it there. A legacy recording is not changed: it is converted for
/// `output`, a new `.dendro`, and the edit goes there; without `output`,
/// `refusal` says how to ask for one. A copy or conversion is staged beside
/// `output` and moved there by [`EditTarget::commit`], so a failed edit
/// leaves nothing at `output`.
pub(crate) fn edit_target(
    input: &Path,
    output: Option<&Path>,
    refusal: impl FnOnce(&Path) -> String,
) -> Result<EditTarget, Box<dyn std::error::Error>> {
    let container = Container::of(input);
    match (container, output) {
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
        (c, None) if c.is_legacy() => Err(format!(
            "{} is a {}; 6.x writes .dendro only and does not change it. Write the \
             result to a new archive instead:\n  {}",
            input.display(),
            c.name(),
            refusal(&dendro_beside(input))
        )
        .into()),
        (c, Some(output)) if c.is_legacy() => {
            check_new_dendro(output)?;
            let (dir, staged) = stage_beside(output)?;
            let summary = convert_quiet(input, &staged)?;
            Ok(EditTarget {
                path: staged,
                note: Some(format!(
                    "converted {} ({}) to {}: {summary}",
                    input.display(),
                    c.name(),
                    output.display()
                )),
                staged: Some((dir, output.to_path_buf())),
            })
        }
        _ => Err(format!(
            "{} is not a recording (.dendro, .rez or .parquet)",
            input.display()
        )
        .into()),
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
