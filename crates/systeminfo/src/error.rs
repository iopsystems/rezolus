use core::fmt;
use std::io;
use std::path::PathBuf;

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

    pub(crate) fn io(error: io::Error) -> Self {
        Self::without_path(ErrorKind::Io, error)
    }
}

#[derive(Debug)]
pub(crate) enum ErrorSource {
    Io(io::Error),
    None,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Io,
}

impl From<io::Error> for ErrorSource {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::None => f.write_str("<no source error>"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Io => write!(f, "io error: {}", self.source),
        }
    }
}
