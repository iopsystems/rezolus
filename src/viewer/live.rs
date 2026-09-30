//! Live mode: an agent's replication stream recorded into a temporary archive
//! and read back while it grows.
//!
//! The viewer used to poll `/metrics/binary`, whose every body carries every
//! acquisition group's full schema, ingest each snapshot into a `MemoryStore`,
//! and keep every raw body in memory for saving. It is now a stream consumer
//! like `record` to a `.dendro` and hindsight: the subscription feeds
//! hindsight's buffer writer, with no retention, and the capture reads the
//! file through a [`LiveReader`] refreshed after each interval. Saving copies
//! the archive.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use metriken_query::BufferPool;
use reqwest::{Client, Url};
use rez::live::LiveReader;
use tracing::{error, info, warn};

use super::actions::AgentInfo;
use crate::hindsight::buffer::HindsightBuffer;
use crate::recorder::stream::{ConnectError, StreamEvent, StreamSchemas, Subscription};

/// How often the live view asks the agent for an interval.
pub const LIVE_INTERVAL: Duration = Duration::from_secs(1);

/// One live agent being recorded. Dropping it stops the recording and
/// deletes the temporary archive.
pub struct LiveSession {
    url: Url,
    info: AgentInfo,
    reader: Arc<LiveReader>,
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    // Declared last so the archive is deleted after the task is stopped.
    _dir: tempfile::TempDir,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LiveSession {
    /// Subscribe to the agent at `url` and start recording it into a new
    /// temporary archive. Must be called inside a Tokio runtime.
    ///
    /// `Err` is an agent that cannot be subscribed to: unreachable, or one
    /// that cannot serve the stream (older than 5.21.0, or a V2 agent).
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
                         Live mode reads /metrics/stream, which agents from 5.21.0 serve.",
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

        let dir = tempfile::Builder::new()
            .prefix("rezolus-live-")
            .tempdir()
            .map_err(|e| format!("could not create a directory for the live archive: {e}"))?;
        let path = dir.path().join("live.dendro");
        // Zero is what no agent anchors at (1970), so it is not an anchor.
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
        // No retention: the view keeps everything since it connected, as
        // the in-memory store it replaces did, but on disk.
        let mut buffer = HindsightBuffer::create_dendro(
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

        let (tx, mut rx) = tokio::sync::mpsc::channel::<(usize, StreamEvent)>(
            crate::recorder::STREAM_QUEUE_PER_ENDPOINT,
        );
        let pump = tokio::spawn(crate::recorder::stream::pump(
            0,
            subscription,
            client.clone(),
            url.clone(),
            LIVE_INTERVAL,
            timeout,
            tx,
        ));
        let live = Arc::clone(&reader);
        let label = url.to_string();
        let task = tokio::spawn(async move {
            // Aborting this task drops the receiver, which ends the pump.
            let _pump = AbortOnDrop(pump);
            let mut schemas = StreamSchemas::default();
            let mut epoch = source.uuid;
            while let Some((_, event)) = rx.recv().await {
                match event {
                    StreamEvent::Interval(applied) => {
                        let written =
                            crate::hindsight::ingest_interval(&mut buffer, &mut schemas, applied)
                                .and_then(|_| buffer.maintain());
                        if let Err(e) = written {
                            error!("{label}: the live recording stopped: {e}");
                            return;
                        }
                        if let Err(e) = live.refresh() {
                            warn!("{label}: could not reread the live archive: {e}");
                        }
                    }
                    StreamEvent::Dropped(e) => {
                        warn!("{label}: the stream ended ({e}); reconnecting");
                    }
                    StreamEvent::Connected(source) => {
                        info!("{label}: the stream reconnected");
                        if source.uuid != epoch {
                            warn!(
                                "{label}: the agent restarted; its counters started again \
                                 from zero"
                            );
                            epoch = source.uuid;
                        }
                    }
                    StreamEvent::Refused(e) => {
                        error!("{label}: the agent can no longer serve its stream: {e}");
                        return;
                    }
                }
            }
        });

        Ok(Self {
            url: url.clone(),
            info,
            reader,
            path,
            task,
            _dir: dir,
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

    /// The temporary archive, which saves copy.
    pub fn path(&self) -> &Path {
        &self.path
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
