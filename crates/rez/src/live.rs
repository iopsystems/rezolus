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

use metriken_query::{
    DisplayOptions, DisplayResult, MetricsSource, QueryError, QueryOptions, QueryResult,
};

use metriken_storage::BufferPool;

use crate::reader::RezReader;

type Error = Box<dyn std::error::Error>;

/// One recording of an archive at a path, or several joined
/// ([`combined`](Self::combined)), reopened on [`refresh`](Self::refresh).
pub struct LiveReader {
    path: PathBuf,
    pool: Arc<BufferPool>,
    /// Which recordings: the full label set of each one first opened. One,
    /// unless `combined`.
    labels: Vec<BTreeMap<String, String>>,
    /// Whether the reader joins every recording of the archive
    /// ([`RezReader::combine`]) rather than reading one.
    combined: bool,
    /// For a combined reader, the label sets of the recordings the current
    /// reader joins, in the archive's order, and how many times that set
    /// has changed since the open ([`recordings`](Self::recordings)).
    joined: std::sync::Mutex<(Vec<BTreeMap<String, String>>, u64)>,
    current: RwLock<Arc<RezReader>>,
    /// The file the current reader is known to read; `None` when that is
    /// not known.
    file: std::sync::Mutex<Option<FileId>>,
    /// What [`MetricsSource::filename`] reports, when not the archive's own.
    name: Option<String>,
}

pub use metriken_storage::rez::file_id::{open_file, FileId};

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
            labels: vec![labels],
            combined: false,
            joined: Default::default(),
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
            labels: vec![labels],
            combined: false,
            joined: Default::default(),
            current: RwLock::new(Arc::new(reader)),
            // Which file `reader` read is not known, so the first refresh
            // does not reuse it.
            file: std::sync::Mutex::new(None),
            name: None,
        }
    }

    /// Wrap `reader`, the recordings labelled `labels` of the archive at
    /// `path` joined by [`RezReader::combine`]. A refresh joins every
    /// recording of the archive again, in the archive's order, and keeps
    /// the current reader when one of `labels` is no longer there.
    pub fn combined(
        path: &Path,
        labels: Vec<BTreeMap<String, String>>,
        reader: RezReader,
        pool: Arc<BufferPool>,
    ) -> Self {
        reader.keep_handover();
        Self {
            path: path.to_path_buf(),
            pool,
            joined: std::sync::Mutex::new((labels.clone(), 0)),
            labels,
            combined: true,
            current: RwLock::new(Arc::new(reader)),
            file: std::sync::Mutex::new(None),
            name: None,
        }
    }

    /// Take this reader's recordings out of `recordings`, a newer open of
    /// the archive at [`path`](Self::path) that read `file`, and make them
    /// the ones queries go to: the recording labelled
    /// [`labels`](Self::labels), or for a [`combined`](Self::combined)
    /// reader every recording, joined. An error, leaving the current reader,
    /// when a recording this reader reads is not among them.
    pub fn take_from(
        &self,
        recordings: &mut crate::reader::LabeledRecordings,
        file: Option<FileId>,
    ) -> Result<(), String> {
        if let Some(missing) = self
            .labels
            .iter()
            .find(|l| !recordings.iter().any(|(r, _)| r == *l))
        {
            return Err(format!("no recording labelled {missing:?}"));
        }
        if self.combined {
            let all = std::mem::take(recordings);
            let now: Vec<BTreeMap<String, String>> = all.iter().map(|(l, _)| l.clone()).collect();
            let (reader, _) = RezReader::combine(all, self.current().filename());
            self.replace(reader, file);
            let mut joined = self.joined.lock().unwrap_or_else(|e| e.into_inner());
            if joined.0 != now {
                *joined = (now, joined.1 + 1);
            }
        } else {
            let at = recordings
                .iter()
                .position(|(l, _)| *l == self.labels[0])
                .expect("checked above");
            self.replace(recordings.swap_remove(at).1, file);
        }
        Ok(())
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
        if self.combined {
            let (opened, file) = open_file(&self.path, || {
                quiet(|| RezReader::open_recordings(&self.path, Arc::clone(&self.pool)))
            });
            return Ok(self.take_from(&mut opened?, file)?);
        }
        let (picked, file) = open_file(&self.path, || {
            quiet(|| pick_exact(&self.path, &self.labels[0], &self.pool))
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

    /// The full label set of the recording this reader reads: the first
    /// one, for a [`combined`](Self::combined) reader.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels[0]
    }

    /// For a [`combined`](Self::combined) reader, the label sets of the
    /// recordings the current reader joins, and a count that grows each
    /// time a refresh finds a different set (a recording added by an
    /// endpoint that came up after the open). `(labels, 0)` otherwise.
    pub fn recordings(&self) -> (Vec<BTreeMap<String, String>>, u64) {
        if !self.combined {
            return (self.labels.clone(), 0);
        }
        self.joined
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The full label set of every recording this reader reads.
    pub fn label_sets(&self) -> &[BTreeMap<String, String>] {
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
    fn query_range_display_opts(
        &self,
        expr: &str,
        start: f64,
        end: f64,
        step: f64,
        opts: &DisplayOptions,
        qopts: &QueryOptions,
    ) -> Result<DisplayResult, QueryError> {
        self.current()
            .query_range_display_opts(expr, start, end, step, opts, qopts)
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
