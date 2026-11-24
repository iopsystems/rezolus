//! A recording an earlier release wrote (`.rez`, `.parquet`,
//! `.parquet.ab.tar`), converted to a `.dendro` when the viewer opens it:
//! a file on the command line, an upload, or a URL load.
//!
//! The conversion is `recording upgrade`'s (`legacy::convert_quiet`),
//! reading from and writing to disk, into one temporary directory per
//! process. [`remove`] deletes it; the viewer calls it when `run` returns,
//! from a panic hook, before each `process::exit` and from the ctrl-c
//! handlers, so only a kill that runs no handler (SIGKILL) leaves it behind. The WASM viewer converts in
//! memory instead (`rez::legacy`), and converts `.rez` and single-source
//! parquet files only.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ::rez::container::Container;

/// The directory converted archives are written to, created on first use.
/// A test has one per thread, removed when the thread ends, so tests that
/// share a process (`cargo test`) do not remove each other's.
#[cfg(not(test))]
static DIR: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);
#[cfg(test)]
thread_local! {
    static DIR: Mutex<Option<tempfile::TempDir>> = const { Mutex::new(None) };
}

/// `f` with the conversion directory, created on first use.
fn with_dir<T>(f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let locked = |dir: &Mutex<Option<tempfile::TempDir>>| {
        let mut dir = dir.lock().unwrap_or_else(|e| e.into_inner());
        if dir.is_none() {
            let created = tempfile::Builder::new()
                .prefix("rezolus-converted-")
                .tempdir()
                .map_err(|e| format!("failed to create a directory to convert into: {e}"))?;
            *dir = Some(created);
        }
        f(dir.as_ref().expect("created above").path())
    };
    #[cfg(not(test))]
    return locked(&DIR);
    #[cfg(test)]
    return DIR.with(locked);
}

/// A path in the conversion directory for a new file named after `stem`,
/// which does not exist yet.
fn fresh_path(stem: &str, extension: &str) -> Result<PathBuf, String> {
    with_dir(|dir| {
        let mut path = dir.join(format!("{stem}.{extension}"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{stem}-{n}.{extension}"));
            n += 1;
        }
        Ok(path)
    })
}

/// Remove the directory converted archives were written to.
pub fn remove() {
    #[cfg(not(test))]
    drop(DIR.lock().unwrap_or_else(|e| e.into_inner()).take());
    #[cfg(test)]
    DIR.with(|dir| drop(dir.lock().unwrap_or_else(|e| e.into_inner()).take()));
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

/// Store the upload `body`, named `filename`, as a dendro archive and
/// return its path: a dendro archive at `dest`, which is replaced, and any
/// other recording converted into the conversion directory, so [`remove`]
/// deletes it. A file that is not a recording is refused.
pub fn stage_upload(body: &[u8], filename: &str, dest: &Path) -> Result<PathBuf, String> {
    let container = Container::of_bytes(body);
    if container == Container::Dendro {
        std::fs::write(dest, body).map_err(|e| format!("failed to store upload: {e}"))?;
        return Ok(dest.to_path_buf());
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
    // Created while the directory is locked, so two uploads at once write
    // two files; removed when `input` drops.
    let input = with_dir(|dir| {
        tempfile::Builder::new()
            .prefix("upload-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(dir)
            .map_err(|e| format!("failed to store upload: {e}"))
    })?;
    std::fs::write(input.path(), body).map_err(|e| format!("failed to store upload: {e}"))?;
    let dest = fresh_path("upload", "dendro")?;
    crate::parquet_tools::legacy::convert_quiet(input.path(), &dest).map_err(|e| {
        // The converter names its input, here a staged copy of the upload.
        let e = e
            .to_string()
            .replace(&input.path().display().to_string(), filename);
        format!("failed to convert {filename} to open it: {e}")
    })?;
    tracing::info!("{}", converted_note(filename, container));
    Ok(dest)
}

#[cfg(test)]
mod tests {
    /// A `.rez` named on the command line is converted into the conversion
    /// directory under its own name, a `.dendro` is opened where it is, and
    /// [`super::remove`] deletes the converted copy.
    #[test]
    fn a_rez_on_the_command_line_is_converted_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let rez = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let converted = super::dendro_path(&rez).unwrap();
        assert_ne!(converted, rez);
        assert_eq!(converted.file_name().unwrap(), "v3.dendro");
        assert_eq!(
            ::rez::container::Container::of_path(&converted),
            ::rez::container::Container::Dendro
        );
        assert_eq!(super::dendro_path(&converted).unwrap(), converted);
        super::remove();
        assert!(!converted.exists(), "removed with the directory");
        let err = super::dendro_path(&dir.path().join("missing.dendro"))
            .err()
            .unwrap();
        assert!(err.contains("not a recording"), "{err}");
    }
}
