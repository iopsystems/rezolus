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
    /// What [`MetricsSource::filename`] reports, when not the archive's own.
    name: Option<String>,
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
        let (labels, reader) = quiet(|| pick(path, selector.as_ref(), &pool))?;
        Ok(Self {
            path: path.to_path_buf(),
            pool,
            labels,
            current: RwLock::new(Arc::new(reader)),
            name: None,
        })
    }

    /// Report `name` as the filename: for a temporary archive whose own name
    /// says nothing, such as the viewer's live mode naming the agent.
    pub fn named(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    /// Reopen the archive, so rows committed since the last open are read.
    /// Logging is off during the reopen, as for [`open`](Self::open).
    ///
    /// The new reader has no decoded blocks cached, so the next query on it
    /// reads the segments it touches again.
    pub fn refresh(&self) -> Result<(), Error> {
        let (_, reader) = quiet(|| pick(&self.path, Some(&self.labels), &self.pool))?;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(reader);
        Ok(())
    }

    /// The reader queries go to now.
    pub fn current(&self) -> Arc<RezReader> {
        Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner()))
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
