//! A recording an earlier release wrote (`.rez`, `.parquet`,
//! `.parquet.ab.tar`), converted to a `.dendro` when the viewer opens it:
//! a file on the command line, an upload, or a URL load.
//!
//! The conversion is `recording upgrade`'s (`legacy::convert_quiet`),
//! reading from and writing to disk, into one temporary directory per
//! process that is removed on exit ([`remove`]). The WASM viewer converts in
//! memory instead (`rez::legacy`), and converts `.rez` and single-source
//! parquet files only.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ::rez::container::Container;

/// The directory converted archives are written to, created on first use.
static DIR: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);

/// A new path in [`DIR`] for a file named after `stem`.
fn fresh_path(stem: &str, extension: &str) -> Result<PathBuf, String> {
    let mut dir = DIR.lock().unwrap_or_else(|e| e.into_inner());
    if dir.is_none() {
        let created = tempfile::Builder::new()
            .prefix("rezolus-converted-")
            .tempdir()
            .map_err(|e| format!("failed to create a directory to convert into: {e}"))?;
        *dir = Some(created);
    }
    let dir = dir.as_ref().expect("created above").path();
    let mut path = dir.join(format!("{stem}.{extension}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{extension}"));
        n += 1;
    }
    Ok(path)
}

/// Remove the directory converted archives were written to. For every exit
/// of the viewer, including one that runs no destructors.
pub fn remove() {
    drop(DIR.lock().unwrap_or_else(|e| e.into_inner()).take());
}

/// The note logged when a file was converted to open it.
fn converted_note(name: &str, container: Container) -> String {
    format!(
        "{name} is a {}, converted to a temporary .dendro to open it; \
         `{} recording upgrade {}` writes a .dendro to keep",
        container.name(),
        env!("CARGO_BIN_NAME"),
        ::rez::container::shell_word(name)
    )
}

/// The dendro archive to open for the file at `path`: `path` itself when it
/// is one, else a conversion of it in the temporary directory, named after
/// the file. A file that is not a recording is refused.
pub fn dendro_path(path: &Path) -> Result<PathBuf, String> {
    let container = Container::of_path(path);
    if container == Container::Dendro {
        return Ok(path.to_path_buf());
    }
    if !container.is_legacy() {
        return Err(::rez::container::not_dendro(
            &path.display().to_string(),
            container,
            env!("CARGO_BIN_NAME"),
        ));
    }
    let name = path.display().to_string();
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            n.trim_end_matches(".parquet.ab.tar")
                .trim_end_matches(".rez")
                .trim_end_matches(".parquet")
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "recording".to_string());
    let out = fresh_path(&stem, "dendro")?;
    crate::parquet_tools::legacy::convert_quiet(path, &out)
        .map_err(|e| format!("failed to convert {name} to open it: {e}"))?;
    tracing::info!("{}", converted_note(&name, container));
    Ok(out)
}

/// Write the upload `body`, named `filename`, to `dest` as a dendro archive:
/// as it is when it is one, else converted from the format it is in. A file
/// that is not a recording is refused. `dest` is replaced.
pub fn stage_upload(body: &[u8], filename: &str, dest: &Path) -> Result<(), String> {
    let container = Container::of_bytes(body);
    if container == Container::Dendro {
        return std::fs::write(dest, body).map_err(|e| format!("failed to store upload: {e}"));
    }
    if !container.is_legacy() {
        return Err(::rez::container::not_dendro(
            filename,
            container,
            env!("CARGO_BIN_NAME"),
        ));
    }
    let extension = match container {
        Container::AbTar => "parquet.ab.tar",
        Container::Parquet => "parquet",
        _ => "rez",
    };
    let input = fresh_path("upload", extension)?;
    std::fs::write(&input, body).map_err(|e| format!("failed to store upload: {e}"))?;
    let _ = std::fs::remove_file(dest);
    let converted = crate::parquet_tools::legacy::convert_quiet(&input, dest)
        .map_err(|e| format!("failed to convert {filename} to open it: {e}"));
    let _ = std::fs::remove_file(&input);
    converted?;
    tracing::info!("{}", converted_note(filename, container));
    Ok(())
}
