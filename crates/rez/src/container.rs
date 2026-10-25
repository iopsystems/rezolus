//! What a recording file is, by content, and the refusal every reader but
//! `recording upgrade` gives a file that is not a `.dendro` archive.
//!
//! Recordings opened anywhere (the viewers, `mcp`, the `recording` tools)
//! are dendro archives. The formats earlier releases wrote (`.rez` archives
//! in both containers, standalone parquet files, `.parquet.ab.tar` A/B
//! tarballs) are converted once, by `recording upgrade`, and the readers
//! refuse them with the command that does it.

use std::io::Read;
use std::path::Path;

/// The container of a recording file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Dendro,
    /// A v3 (SQLite) `.rez`.
    RezV3,
    /// A v1 or v2 (tar) `.rez`.
    RezTar,
    Parquet,
    /// A `.parquet.ab.tar` A/B tarball: two parquet files and an `ab.json`.
    AbTar,
    /// Anything else, or a file that could not be read.
    Unknown,
}

/// The name of the A/B tarball's manifest entry.
const AB_MANIFEST_NAME: &str = "ab.json";

impl Container {
    /// The container of the file at `path`.
    pub fn of_path(path: &Path) -> Self {
        use metriken_storage::rez::rez::{detect_rez_format, RezFormat};
        if metriken_storage::DendroCatalog::is_archive(path).unwrap_or(false) {
            return Self::Dendro;
        }
        match detect_rez_format(path) {
            Ok(RezFormat::V3Sqlite) => return Self::RezV3,
            Ok(RezFormat::V2Tar) => return Self::RezTar,
            _ => {}
        }
        let mut head = vec![0u8; 512];
        let read = std::fs::File::open(path).and_then(|mut f| {
            let mut n = 0;
            while n < head.len() {
                match f.read(&mut head[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            Ok(n)
        });
        match read {
            Ok(n) => {
                head.truncate(n);
                if head.starts_with(b"PAR1") {
                    Self::Parquet
                } else if is_ab_tar_path(path) {
                    Self::AbTar
                } else {
                    Self::Unknown
                }
            }
            Err(_) => Self::Unknown,
        }
    }

    /// The container of a file held as `bytes`.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        use metriken_storage::rez::catalog::Container as Catalog;
        use metriken_storage::rez::rez::{detect_rez_format_bytes, RezFormat};
        if Catalog::Dendro.recognizes_bytes(bytes) {
            return Self::Dendro;
        }
        match detect_rez_format_bytes(bytes) {
            RezFormat::V3Sqlite => return Self::RezV3,
            RezFormat::V2Tar => return Self::RezTar,
            _ => {}
        }
        if bytes.starts_with(b"PAR1") {
            Self::Parquet
        } else if is_ab_tar(std::io::Cursor::new(bytes)) {
            Self::AbTar
        } else {
            Self::Unknown
        }
    }

    /// Whether this is a format an earlier release wrote, which
    /// `recording upgrade` converts.
    pub fn is_legacy(self) -> bool {
        matches!(
            self,
            Self::RezV3 | Self::RezTar | Self::Parquet | Self::AbTar
        )
    }

    /// The format's name in a message.
    pub fn name(self) -> &'static str {
        match self {
            Self::Dendro => "dendro archive",
            Self::RezV3 => "v3 .rez archive",
            Self::RezTar => "v1/v2 (tar) .rez archive",
            Self::Parquet => "parquet recording",
            Self::AbTar => ".parquet.ab.tar A/B tarball",
            Self::Unknown => "file",
        }
    }
}

fn is_ab_tar_path(path: &Path) -> bool {
    std::fs::File::open(path).is_ok_and(is_ab_tar)
}

/// Whether `reader` is a tar archive holding an `ab.json` entry.
fn is_ab_tar(reader: impl Read) -> bool {
    let mut archive = tar::Archive::new(reader);
    let Ok(entries) = archive.entries() else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let named = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == AB_MANIFEST_NAME))
            .unwrap_or(false);
        if named {
            return true;
        }
    }
    false
}

/// The refusal for a file that is not a `.dendro` archive: for a format an
/// earlier release wrote, the `upgrade` command that converts it, spelled
/// with `program`; for anything else, what is expected.
pub fn not_dendro(path: &str, container: Container, program: &str) -> String {
    if container.is_legacy() {
        format!(
            "{path} is a {}, which {program} no longer opens; convert it to a .dendro \
             archive first:\n  {program} recording upgrade {}",
            container.name(),
            shell_word(path)
        )
    } else {
        format!("{path} is not a recording (a .dendro archive)")
    }
}

/// `Ok` when the file at `path` is a dendro archive, else the refusal
/// [`not_dendro`] gives it.
pub fn require_dendro(path: &Path, program: &str) -> Result<(), String> {
    match Container::of_path(path) {
        Container::Dendro => Ok(()),
        c => Err(not_dendro(&path.display().to_string(), c, program)),
    }
}

/// The refusal of a file holding `n` recordings as one side (`side`, "the
/// baseline" or "the experiment") of a two-file comparison, where each file
/// is one recording. It names the commands that compare two of them: put
/// both files' recordings in one archive, list them, and choose the two by
/// labels or, where labels cannot tell them apart, by position.
pub fn one_recording_hint(
    program: &str,
    file: &str,
    side: &str,
    n: usize,
    baseline: &str,
    experiment: &str,
) -> String {
    if n == 0 {
        return format!("{file} holds no recordings");
    }
    format!(
        "{file} holds {n} recordings; {side} of a two-file comparison is one recording. \
         Put the recordings in one archive and choose the two to compare:\n  \
         {program} recording combine {} {} -o both.dendro\n  \
         {program} mcp describe-recording both.dendro\n  \
         {program} view both.dendro --baseline KEY=VALUE --experiment KEY=VALUE\n\
         If describe-recording says a recording cannot be selected by labels, label \
         the two by the positions it lists and view that archive:\n  \
         {program} recording combine both.dendro --ab baseline=N experiment=M -o ab.dendro\n  \
         {program} view ab.dendro",
        shell_word(baseline),
        shell_word(experiment),
    )
}

/// `word` quoted for a POSIX shell when it needs it: one that is safe bare
/// (letters, digits and `_-.,:/=@%+`) is returned as it is, anything else
/// single-quoted.
pub fn shell_word(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-.,:/=@%+".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rez")
            .join(name)
    }

    /// Each committed legacy fixture is named for what it is, from its path
    /// and from its bytes alike, and refused with the upgrade command.
    #[test]
    fn legacy_files_are_named_and_refused_with_the_upgrade_command() {
        for (name, want) in [
            ("v2.rez", Container::RezTar),
            ("v3.rez", Container::RezV3),
            ("v3-live.rez", Container::RezV3),
        ] {
            let path = committed(name);
            assert_eq!(Container::of_path(&path), want, "{name}");
            assert_eq!(
                Container::of_bytes(&std::fs::read(&path).unwrap()),
                want,
                "{name}"
            );
            let err = require_dendro(&path, "tool").unwrap_err();
            assert!(
                err.ends_with(&format!("tool recording upgrade {}", path.display())),
                "{err}"
            );
        }
        assert_eq!(Container::of_bytes(b"PAR1....PAR1"), Container::Parquet);
        assert_eq!(Container::of_bytes(b"not a recording"), Container::Unknown);
        assert!(!Container::Unknown.is_legacy());
    }

    #[test]
    fn shell_word_quotes_only_what_needs_it() {
        assert_eq!(shell_word("run.rez"), "run.rez");
        assert_eq!(shell_word("my run.rez"), "'my run.rez'");
        assert_eq!(shell_word("it's.rez"), "'it'\\''s.rez'");
    }
}
