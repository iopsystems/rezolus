//! Live mode: an agent's replication stream recorded into a temporary archive
//! and read back while it grows.
//!
//! A [`LiveSession`] subscribes to the agent's `/metrics/stream`, writes each
//! interval into a temporary `.dendro` through hindsight's buffer writer with
//! no retention, and serves the archive through a [`LiveReader`] refreshed
//! after each interval. Saving copies the archive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metriken_query::BufferPool;
use reqwest::{Client, Url};
use rez::live::LiveReader;
use tracing::{error, info, warn};

use super::actions::AgentInfo;
use crate::hindsight::buffer::HindsightBuffer;
use crate::recorder::stream::{ConnectError, StreamEvent, StreamSchemas, Subscription};

/// The interval the live view subscribes at.
pub const LIVE_INTERVAL: Duration = Duration::from_secs(1);

/// Every live archive directory that exists, so an exit that runs no
/// destructors (`std::process::exit` from the Ctrl-C handler) can remove
/// them; see [`remove_live_dirs`].
static LIVE_DIRS: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Remove every live archive directory. For an exit path that skips
/// destructors.
pub fn remove_live_dirs() {
    let dirs = std::mem::take(&mut *LIVE_DIRS.lock().unwrap_or_else(|e| e.into_inner()));
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A live archive's directory, deleted when the last holder drops it: the
/// session, its recording thread, and any save copying the archive.
pub struct LiveDir(tempfile::TempDir);

impl LiveDir {
    fn create() -> std::io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("rezolus-live-").tempdir()?;
        LIVE_DIRS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.path().to_path_buf());
        Ok(Self(dir))
    }
}

impl Drop for LiveDir {
    fn drop(&mut self) {
        LIVE_DIRS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.0.path());
    }
}

/// One live agent being recorded. Dropping it stops the recording; the
/// archive is deleted once the recording thread and any save in progress
/// have let go of it.
pub struct LiveSession {
    url: Url,
    info: AgentInfo,
    reader: Arc<LiveReader>,
    path: PathBuf,
    dir: Arc<LiveDir>,
    /// Aborted on drop, which closes the channel and ends the recording
    /// thread.
    _pump: AbortOnDrop,
}

impl LiveSession {
    /// Subscribe to the agent at `url` and start recording it into a new
    /// temporary archive. Must be called inside a Tokio runtime.
    ///
    /// `Err` is an agent that cannot be subscribed to: unreachable, or one
    /// that cannot serve the stream (older than 5.21.0, or one with
    /// `snapshot_format = "v2"`).
    pub async fn start(
        client: &Client,
        url: &Url,
        info: AgentInfo,
        pool: Arc<BufferPool>,
    ) -> Result<Self, String> {
        let timeout = crate::recorder::tick_timeout(LIVE_INTERVAL);
        let subscription =
            match tokio::time::timeout(timeout, Subscription::connect(client, url, LIVE_INTERVAL))
                .await
            {
                Ok(Ok(sub)) => sub,
                Ok(Err(ConnectError::Unsupported(e))) => {
                    return Err(format!(
                        "the agent at {url} ({}) cannot serve its replication stream: {e}. \
                         Live mode reads /metrics/stream, which agents from 5.21.0 serve \
                         when snapshot_format is \"v3\".",
                        info.version
                    ))
                }
                Ok(Err(ConnectError::Unreachable(e))) => {
                    return Err(format!("could not subscribe to the agent at {url}: {e}"))
                }
                Err(_) => {
                    return Err(format!(
                        "could not subscribe to the agent at {url}: no handshake within {}",
                        humantime::format_duration(timeout)
                    ))
                }
            };
        let source = subscription
            .source()
            .cloned()
            .expect("connect returns only once the handshake has been applied");

        let dir = Arc::new(
            LiveDir::create()
                .map_err(|e| format!("could not create a directory for the live archive: {e}"))?,
        );
        let path = dir.0.path().join("live.dendro");
        // An anchor of 0 means the handshake carried none; use the local clock.
        let clock_anchor_wall_ns = u64::try_from(source.clock_anchor_wall_ns)
            .ok()
            .filter(|a| *a != 0)
            .unwrap_or_else(wall_ns);
        let seed = crate::recorder::rez_v3_writer::ManifestSeed {
            labels: crate::recorder::rez::build_labels("rezolus", info.sysinfo.as_deref(), &[]),
            metadata: crate::hindsight::buffer_metadata(
                LIVE_INTERVAL,
                &info.sysinfo,
                &None,
                &Some(info.version.clone()).filter(|v| !v.is_empty()),
                &source.uuid,
            ),
            clock_anchor_wall_ns,
        };
        // No retention: the archive holds everything since the session
        // started.
        let buffer = HindsightBuffer::create_dendro(
            &path,
            seed,
            Duration::MAX,
            crate::recorder::seal_policy::SealPolicy::default(),
        )?;
        let reader = Arc::new(
            LiveReader::open(&path, None, pool)
                .map_err(|e| format!("could not read the live archive: {e}"))?
                .named(url.to_string()),
        );

        let (tx, rx) = tokio::sync::mpsc::channel::<(usize, StreamEvent)>(
            crate::recorder::STREAM_QUEUE_PER_ENDPOINT,
        );
        let pump = AbortOnDrop(tokio::spawn(crate::recorder::stream::pump(
            0,
            subscription,
            client.clone(),
            url.clone(),
            LIVE_INTERVAL,
            timeout,
            tx,
        )));
        // The writes, seals and reopens block, so they run on their own
        // thread rather than on the runtime that serves the viewer's HTTP.
        let recording = Recording {
            label: url.to_string(),
            buffer,
            reader: Arc::clone(&reader),
            epoch: source.uuid,
            dir: Arc::clone(&dir),
        };
        std::thread::Builder::new()
            .name("rezolus-live".to_string())
            .spawn(move || recording.run(rx))
            .map_err(|e| format!("could not start the live recording: {e}"))?;

        Ok(Self {
            url: url.clone(),
            info,
            reader,
            path,
            dir,
            _pump: pump,
        })
    }

    /// The agent this session records, for starting a fresh one against it:
    /// the viewer's reset.
    pub fn target(&self) -> (Url, AgentInfo) {
        (self.url.clone(), self.info.clone())
    }

    /// The capture's data source.
    pub fn reader(&self) -> Arc<LiveReader> {
        Arc::clone(&self.reader)
    }

    /// The temporary archive's path, which a save copies.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The archive's path and a hold on its directory. A save keeps the hold
    /// until its copy is done, so a reset during the copy does not delete
    /// the archive under it.
    pub fn archive(&self) -> (PathBuf, Arc<LiveDir>) {
        (self.path.clone(), Arc::clone(&self.dir))
    }
}

/// The recording thread's state.
struct Recording {
    label: String,
    buffer: HindsightBuffer,
    reader: Arc<LiveReader>,
    epoch: Option<String>,
    dir: Arc<LiveDir>,
}

impl Recording {
    /// Write each interval as it arrives, until the channel closes (the
    /// session was dropped), a write fails, or the agent refuses a reconnect.
    fn run(mut self, mut rx: tokio::sync::mpsc::Receiver<(usize, StreamEvent)>) {
        let label = self.label.clone();
        let mut schemas = StreamSchemas::default();
        while let Some((_, event)) = rx.blocking_recv() {
            match event {
                StreamEvent::Interval(applied) => {
                    let written =
                        crate::hindsight::ingest_interval(&mut self.buffer, &mut schemas, applied)
                            .and_then(|_| self.buffer.maintain());
                    if let Err(e) = written {
                        error!("{label}: the live recording stopped: {e}");
                        break;
                    }
                    if let Err(e) = self.reader.refresh() {
                        warn!("{label}: could not reread the live archive: {e}");
                    }
                }
                StreamEvent::Dropped(e) => {
                    warn!("{label}: the stream ended ({e}); reconnecting");
                }
                StreamEvent::Connected(source) => {
                    info!("{label}: the stream reconnected");
                    if source.uuid != self.epoch {
                        warn!("{label}: the agent restarted; its counters started again from zero");
                        self.epoch = source.uuid;
                    }
                }
                StreamEvent::Refused(e) => {
                    error!("{label}: the agent can no longer serve its stream: {e}");
                    break;
                }
            }
        }
        // The writer closes before the directory can be removed.
        let Recording { buffer, dir, .. } = self;
        drop(buffer);
        drop(dir);
    }
}

/// Aborts the task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn wall_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dendro_copy::fixtures::{tick, ANCHOR, SECOND};
    use metriken_query::MetricsSource;

    fn buffer(path: &Path) -> HindsightBuffer {
        let seed = crate::recorder::rez_v3_writer::ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: ANCHOR,
        };
        let policy = crate::recorder::seal_policy::SealPolicy {
            max_rows: 4,
            ..Default::default()
        };
        HindsightBuffer::create_dendro(path, seed, Duration::MAX, policy).unwrap()
    }

    fn record(buffer: &mut HindsightBuffer, ticks: std::ops::Range<u64>) {
        for i in ticks {
            buffer.ingest(&tick(i), ANCHOR + i * SECOND, 0).unwrap();
            buffer.maintain().unwrap();
        }
        buffer.sync().unwrap();
    }

    /// Live mode opens its reader before the first interval arrives, so an
    /// archive with a source and no rows has to open, and a refresh then
    /// finds the rows.
    #[test]
    fn a_reader_opened_before_any_rows_finds_them_on_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let mut buffer = buffer(&path);
        buffer.sync().unwrap();
        let live = LiveReader::open(&path, None, BufferPool::new(64 << 20)).unwrap();
        assert_eq!(live.time_range_ns(), None);

        record(&mut buffer, 0..3);
        live.refresh().unwrap();
        assert_eq!(live.time_range_ns(), Some((ANCHOR, ANCHOR + 2 * SECOND)));
    }

    /// An open reader's view is fixed: rows committed after it opened are
    /// not in it until it is refreshed, and then they are, sealed or not.
    #[test]
    fn a_refresh_reads_what_was_committed_since() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let mut buffer = buffer(&path);
        record(&mut buffer, 0..3);
        let live = LiveReader::open(&path, None, BufferPool::new(64 << 20)).unwrap();
        let before = live.time_range_ns();
        assert_eq!(before, Some((ANCHOR, ANCHOR + 2 * SECOND)));

        // Ten more ticks: segments seal every four rows, and the rest stay in
        // the WAL.
        record(&mut buffer, 3..13);
        assert_eq!(live.time_range_ns(), before, "fixed until refreshed");

        live.refresh().unwrap();
        assert_eq!(live.time_range_ns(), Some((ANCHOR, ANCHOR + 12 * SECOND)));
        // The last tick's gauge, which was in the WAL when refreshed:
        // `mem_free` is 1,000 - i.
        let at = (ANCHOR + 12 * SECOND) as f64 / 1e9;
        let metriken_query::QueryResult::Vector { result } =
            live.query("mem_free", Some(at)).unwrap()
        else {
            panic!("an instant query gives a vector");
        };
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].value.1, 988.0);
    }

    /// A selector matches a recording whose labels include every pair in it,
    /// as the `--recording` selectors do, and one that matches none is an
    /// error.
    #[test]
    fn a_selector_matches_by_subset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let mut buffer = buffer(&path);
        record(&mut buffer, 0..1);
        let subset = [("source".to_string(), "rezolus".to_string())].into();
        assert!(LiveReader::open(&path, Some(subset), BufferPool::new(64 << 20)).is_ok());
        let other = [("source".to_string(), "elsewhere".to_string())].into();
        let err = LiveReader::open(&path, Some(other), BufferPool::new(64 << 20))
            .err()
            .expect("no recording matches");
        assert!(err.to_string().contains("no recording matching"), "{err}");
    }

    /// The live view names the agent, not the temporary file.
    #[test]
    fn a_named_reader_reports_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let mut buffer = buffer(&path);
        record(&mut buffer, 0..1);
        let live = LiveReader::open(&path, None, BufferPool::new(64 << 20))
            .unwrap()
            .named("http://agent:4241/".to_string());
        assert_eq!(live.filename().as_deref(), Some("http://agent:4241/"));
    }
}
