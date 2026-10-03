//! A reader of an archive that is still being written.
//!
//! An [`ArchiveReader`](crate::reader::ArchiveReader)'s view is fixed when it
//! opens: its tables, their spans and the WAL tail it materializes. Rows a
//! writer commits afterwards are not in it, and its time range does not grow.
//! [`LiveReader`] holds one recording's reader and replaces it with a fresh
//! one on [`refresh`](LiveReader::refresh). Reopening reads the catalog and
//! one footer per table, not the segments.
//!
//! Queries always go to the current reader. A query that started before a
//! refresh finishes on the reader it started with.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use metriken_query::{BufferPool, MetricsSource, QueryError, QueryOptions, QueryResult};

use crate::reader::RezReader;

type Error = Box<dyn std::error::Error>;

/// One recording of an archive at a path, reopened on [`refresh`](Self::refresh).
pub struct LiveReader {
    path: PathBuf,
    pool: Arc<BufferPool>,
    /// Which recording: the full label set of the one first opened.
    labels: BTreeMap<String, String>,
    current: RwLock<Arc<RezReader>>,
    /// The file the current reader is known to read; `None` when that is
    /// not known.
    file: std::sync::Mutex<Option<FileId>>,
    /// What [`MetricsSource::filename`] reports, when not the archive's own.
    name: Option<String>,
}

/// The device and inode of a file. A file renamed over another, as
/// `recording filter` does to its input, has a different one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId(u64, u64);

impl FileId {
    /// The id of the file at `path`; `None` when it cannot be read, and on a
    /// platform without inodes.
    pub(crate) fn of(path: &Path) -> Option<FileId> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = std::fs::metadata(path).ok()?;
            Some(FileId(m.dev(), m.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            None
        }
    }
}

/// Run `open` on the file at `path`, and return its result with the file's
/// id when the file at `path` was the same before and after the open; `None`
/// when it changed, or cannot be told. A reader opened from a path reads its
/// tables from the path again on their first query, and refuses to once
/// another file is there (see `Container::reopen`), so this is the only file
/// the reader reads.
pub fn open_file<T>(path: &Path, open: impl FnOnce() -> T) -> (T, Option<FileId>) {
    let before = FileId::of(path);
    let opened = open();
    let after = FileId::of(path);
    (opened, before.filter(|b| Some(*b) == after))
}

impl LiveReader {
    /// Open one recording of the archive at `path`: the one whose labels
    /// include every pair in `selector`, or the only recording when
    /// `selector` is `None`. A selector that matches no recording or several
    /// is an error, as the `--recording` selectors are.
    ///
    /// Opening an archive that is not finalized is logged as a warning by
    /// the reader. A live archive is never finalized, so this and
    /// [`refresh`](Self::refresh) open it with logging off: every event
    /// emitted on this thread during the open is dropped. A failed open is
    /// returned as `Err`.
    pub fn open(
        path: &Path,
        selector: Option<BTreeMap<String, String>>,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Error> {
        let (picked, file) = open_file(path, || quiet(|| pick(path, selector.as_ref(), &pool)));
        let (labels, reader) = picked?;
        reader.keep_handover();
        Ok(Self {
            path: path.to_path_buf(),
            pool,
            labels,
            current: RwLock::new(Arc::new(reader)),
            file: std::sync::Mutex::new(file),
            name: None,
        })
    }

    /// Wrap `reader`, already open on the recording labelled `labels` of the
    /// archive at `path`. For a caller that opened every recording once and
    /// chose which to show, such as the viewer's file mode: the archive is
    /// not opened again, and [`refresh`](Self::refresh) finds the recording
    /// by `labels` exactly, as it does after [`open`](Self::open).
    ///
    /// A refresh takes the first recording whose label set equals `labels`.
    /// When two recordings of the archive have the same label set, both
    /// readers wrapping them read the first one after a refresh, so a caller
    /// must not wrap recordings whose label sets are equal.
    pub fn from_reader(
        path: &Path,
        labels: BTreeMap<String, String>,
        reader: RezReader,
        pool: Arc<BufferPool>,
    ) -> Self {
        reader.keep_handover();
        Self {
            path: path.to_path_buf(),
            pool,
            labels,
            current: RwLock::new(Arc::new(reader)),
            // Which file `reader` read is not known, so the first refresh
            // does not reuse it.
            file: std::sync::Mutex::new(None),
            name: None,
        }
    }

    /// Report `name` as the filename: for a temporary archive whose own name
    /// says nothing, such as the viewer's live mode naming the agent.
    pub fn named(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    /// Reopen the archive, so rows committed since the last open are read.
    /// Logging is off during the reopen, as for [`open`](Self::open). The
    /// recording is found again by the full label set it had when opened.
    ///
    /// A reopen opens every recording of the archive and reads the catalog;
    /// a table is built on its first query after the reopen, from the state
    /// the previous reader saved for it when both reads were of the same
    /// file (see [`replace`](Self::replace)). A caller
    /// refreshing several readers of one archive can open it once and hand
    /// each its recording with [`replace`](Self::replace).
    pub fn refresh(&self) -> Result<(), Error> {
        let (picked, file) = open_file(&self.path, || {
            quiet(|| pick_exact(&self.path, &self.labels, &self.pool))
        });
        self.replace(picked?.1, file);
        Ok(())
    }

    /// Make `reader` the one queries go to. It must be a newer open of this
    /// reader's recording: the one labelled [`labels`](Self::labels) in the
    /// archive at [`path`](Self::path). `file` is the file it read, as
    /// [`open_file`] reports it.
    ///
    /// When `file` is the file the current reader read, `reader` starts
    /// each table from the state the current reader saved for it
    /// (`ArchiveReader::reuse_from`): a table whose previous reader saved
    /// state reads only the segments sealed since and the live tail.
    pub fn replace(&self, reader: RezReader, file: Option<FileId>) {
        let mut current_file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if file.is_some() && file == *current_file {
            reader.reuse_from(&self.current());
        } else {
            reader.keep_handover();
        }
        *current_file = file;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(reader);
    }

    /// The full label set of the recording this reader reads.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    /// The reader queries go to now.
    pub fn current(&self) -> Arc<RezReader> {
        Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whether the recording was finalized as of the last open. `record`
    /// finalizes on a clean exit; a killed writer's file, or a `cp` or
    /// `recording snapshot` of a running archive, is not finalized.
    pub fn complete(&self) -> bool {
        self.current().complete()
    }

    /// The archive's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Run `f` with every tracing event on this thread dropped.
fn quiet<T>(f: impl FnOnce() -> T) -> T {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), f)
}

/// The recording whose label set equals `labels`.
fn pick_exact(
    path: &Path,
    labels: &BTreeMap<String, String>,
    pool: &Arc<BufferPool>,
) -> Result<(BTreeMap<String, String>, RezReader), Error> {
    let mut recordings = RezReader::open_recordings(path, Arc::clone(pool))?;
    let at = recordings
        .iter()
        .position(|(l, _)| l == labels)
        .ok_or_else(|| format!("{} has no recording labelled {labels:?}", path.display()))?;
    Ok(recordings.swap_remove(at))
}

fn pick(
    path: &Path,
    selector: Option<&BTreeMap<String, String>>,
    pool: &Arc<BufferPool>,
) -> Result<(BTreeMap<String, String>, RezReader), Error> {
    let mut recordings = RezReader::open_recordings(path, Arc::clone(pool))?;
    let matching: Vec<usize> = recordings
        .iter()
        .enumerate()
        .filter(|(_, (labels, _))| {
            selector.is_none_or(|sel| sel.iter().all(|(k, v)| labels.get(k) == Some(v)))
        })
        .map(|(i, _)| i)
        .collect();
    match matching.as_slice() {
        [at] => Ok(recordings.swap_remove(*at)),
        [] => Err(match selector {
            Some(sel) => format!("{} has no recording matching {sel:?}", path.display()),
            None => format!("{} holds no recording", path.display()),
        }
        .into()),
        several => Err(format!(
            "{} holds {} recordings matching {selector:?}; name one by its labels",
            path.display(),
            several.len()
        )
        .into()),
    }
}

impl MetricsSource for LiveReader {
    fn query_range_opts(
        &self,
        expr: &str,
        start: f64,
        end: f64,
        step: f64,
        opts: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        self.current()
            .query_range_opts(expr, start, end, step, opts)
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        self.current().query(expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        self.current().columns(query)
    }
    fn counter_names(&self) -> Vec<String> {
        self.current().counter_names()
    }
    fn gauge_names(&self) -> Vec<String> {
        self.current().gauge_names()
    }
    fn histogram_names(&self) -> Vec<String> {
        self.current().histogram_names()
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.current().counter_labels(name)
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.current().gauge_labels(name)
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.current().histogram_labels(name)
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        self.current().time_range()
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.current().time_range_ns()
    }
    fn interval(&self) -> f64 {
        self.current().interval()
    }
    fn source(&self) -> String {
        self.current().source()
    }
    fn version(&self) -> String {
        self.current().version()
    }
    fn filename(&self) -> Option<String> {
        self.name.clone().or_else(|| self.current().filename())
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.current().metadata_get(key)
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.current().file_metadata()
    }
    fn sample_timestamps(&self) -> Vec<u64> {
        self.current().sample_timestamps()
    }
}
