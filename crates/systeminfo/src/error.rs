use core::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    path: Option<PathBuf>,
    source: ErrorSource,
    kind: ErrorKind,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, source: ErrorSource, path: Option<PathBuf>) -> Self {
        Self { kind, source, path }
    }

    pub(crate) fn with_path(
        kind: ErrorKind,
        source: impl Into<ErrorSource>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self::new(kind, source.into(), Some(path.into()))
    }

    pub(crate) fn without_path(kind: ErrorKind, source: impl Into<ErrorSource>) -> Self {
        Self::new(kind, source.into(), None)
    }

    fn path(&self) -> &Path {
        match &self.path {
            Some(path) => path,
            None => Path::new("<missing>"),
        }
    }

    pub(crate) fn unreadable(error: io::Error, path: impl AsRef<Path>) -> Self {
        Self::with_path(ErrorKind::UnreadableFile, error, path.as_ref())
    }

    pub(crate) fn unparseable(error: impl Into<ErrorSource>, path: impl AsRef<Path>) -> Self {
        Self::with_path(ErrorKind::UnparseableFile, error, path.as_ref())
    }

    pub(crate) fn invalid_interface_name() -> Self {
        Self::without_path(ErrorKind::InvalidInterfaceName, ErrorSource::None)
    }
}

#[derive(Debug)]
pub(crate) enum ErrorSource {
    Io(io::Error),
    ParseInt(std::num::ParseIntError),
    None,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    UnreadableFile,
    UnparseableFile,
    InvalidInterfaceName,
}

impl fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::ParseInt(e) => e.fmt(f),
            Self::None => f.write_str("<no source error>"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::UnreadableFile => write!(
                f,
                "could not read {}: {}",
                self.path().display(),
                self.source
            ),
            ErrorKind::UnparseableFile => write!(
                f,
                "could not parse the contents of {}",
                self.path().display()
            ),
            ErrorKind::InvalidInterfaceName => f.write_str("interface name was not valid UTF-8"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            ErrorSource::Io(err) => Some(err),
            ErrorSource::ParseInt(err) => Some(err),
            ErrorSource::None => None,
        }
    }
}

impl From<io::Error> for ErrorSource {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<std::num::ParseIntError> for ErrorSource {
    fn from(value: std::num::ParseIntError) -> Self {
        Self::ParseInt(value)
    }
}
