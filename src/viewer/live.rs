//! Live mode: an agent's replication stream recorded into a temporary archive
//! and read back while it grows.
//!
//! A [`LiveSession`] records the agent's `/metrics/stream` into a temporary
//! `.dendro` through `metriken-recorder` with no retention, and serves the
//! archive through a [`LiveReader`] refreshed after each commit. Saving copies
//! the archive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metriken_storage::BufferPool;
use reqwest::{Client, Url};
use rez::live::LiveReader;
use tracing::{error, info, warn};

use super::actions::AgentInfo;
use metriken_recorder as mr;

/// The interval the live view subscribes at.
pub const LIVE_INTERVAL: Duration = Duration::from_secs(1);

/// Every live archive directory that exists, so an exit that runs no
/// destructors (`std::process::exit` from the Ctrl-C handler) can remove
/// them; see [`remove_live_dirs`].
static LIVE_DIRS: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Every live recording thread started in this process, so a normal exit can
/// wait for them to close their writers; see [`finish_live_sessions`].
static LIVE_THREADS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Wait up to `within` for every live recording thread to finish, then remove
/// every live archive directory. For a normal exit after the sessions were
/// dropped: a thread finishes once its channel closes, and the directory is
/// removed after its writer has closed, so no file appears in it mid-removal.
/// A thread still running at the deadline is left running, and its directory
/// is removed regardless.
pub fn finish_live_sessions(within: Duration) {
    let threads = std::mem::take(&mut *LIVE_THREADS.lock().unwrap_or_else(|e| e.into_inner()));
    let deadline = std::time::Instant::now() + within;
    for thread in threads {
        while !thread.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
    remove_live_dirs();
}

/// Remove every live archive directory. For an exit while sessions are still
/// alive, whose recording threads will not finish first.
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
    /// Why the recording stopped, once it has: a failed write, or an agent
    /// that refused a reconnect.
    stopped: Arc<Mutex<Option<String>>>,
    /// Dropped with the session, which stops the recording and closes its
    /// event channel, ending the recording thread.
    _recorder: Option<mr::Recorder>,
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
        let probe: Arc<dyn mr::Probe> = Arc::new(crate::recorder::agent_probe::RezolusProbe::new(
            client.clone(),
        ));
        let spec = mr::EndpointSpec::new(url.clone()).with_transport(mr::Transport::Stream);
        let mut detect_cfg = mr::DetectConfig::new(LIVE_INTERVAL, timeout);
        detect_cfg.wire_names = crate::recorder::stream::ACCEPTED_WIRE_NAMES.to_vec();
        let detected = mr::detect(client, &detect_cfg, vec![spec], probe.as_ref()).await;
        match &detected[0] {
            mr::Detected::Streamed { .. } => {}
            mr::Detected::Refused { reason, .. } => {
                return Err(format!(
                    "the agent at {url} ({}) cannot serve its replication stream: {reason}. \
                     Live mode reads /metrics/stream, which agents from 5.21.0 serve \
                     when snapshot_format is \"v3\".",
                    info.version
                ))
            }
            mr::Detected::Pending { reason, .. } => {
                return Err(format!(
                    "could not subscribe to the agent at {url}: {reason}"
                ))
            }
            mr::Detected::Scraped { .. } => unreachable!("a declared stream is never scraped"),
        }

        let dir = Arc::new(
            LiveDir::create()
                .map_err(|e| format!("could not create a directory for the live archive: {e}"))?,
        );
        let path = dir.0.path().join("live.dendro");
        // No retention: the archive holds everything since the session
        // started.
        let mut cfg = mr::RecorderConfig::new(&path, LIVE_INTERVAL);
        cfg.timeout = timeout;
        cfg.wire_names = crate::recorder::stream::ACCEPTED_WIRE_NAMES.to_vec();
        let (recorder, _ids, events) =
            mr::Recorder::start(cfg, client.clone(), probe, detected, Vec::new())
                .await
                .map_err(|e| format!("could not start the live recording: {e}"))?;
        let reader = Arc::new(
            LiveReader::open(&path, None, pool)
                .map_err(|e| format!("could not read the live archive: {e}"))?
                .named(url.to_string()),
        );

        // The reader's reopens block, so they run on their own thread
        // rather than on the runtime that serves the viewer's HTTP.
        let stopped = Arc::new(Mutex::new(None));
        let recording = Recording {
            label: url.to_string(),
            reader: Arc::clone(&reader),
            dir: Arc::clone(&dir),
            stopped: Arc::clone(&stopped),
        };
        let thread = std::thread::Builder::new()
            .name("rezolus-live".to_string())
            .spawn(move || recording.run(events))
            .map_err(|e| format!("could not start the live recording: {e}"))?;
        {
            // Join the threads of sessions already ended, so the list holds
            // only live ones.
            let mut threads = LIVE_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            let (done, live): (Vec<_>, Vec<_>) = threads.drain(..).partition(|t| t.is_finished());
            for t in done {
                let _ = t.join();
            }
            *threads = live;
            threads.push(thread);
        }

        Ok(Self {
            url: url.clone(),
            info,
            reader,
            path,
            dir,
            stopped,
            _recorder: Some(recorder),
        })
    }

    /// Why the recording stopped, if it has. The view then no longer
    /// advances; a reset starts a new session.
    pub fn stopped(&self) -> Option<String> {
        self.stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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

    /// The temporary archive's path. A save uses [`archive`](Self::archive),
    /// which also holds the directory.
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

#[cfg(test)]
impl LiveSession {
    /// A session over an existing archive, with no subscription or
    /// recording thread, whose recording reports `stopped`. Must be called
    /// inside a Tokio runtime.
    pub(crate) fn for_test(path: &Path, stopped: Option<String>) -> Self {
        let reader = Arc::new(LiveReader::open(path, None, BufferPool::new(64 << 20)).unwrap());
        Self {
            url: "http://agent.test:4241/".parse().unwrap(),
            info: AgentInfo {
                source: "rezolus".to_string(),
                version: "test".to_string(),
                sysinfo: None,
            },
            reader,
            path: path.to_path_buf(),
            dir: Arc::new(LiveDir::create().unwrap()),
            stopped: Arc::new(Mutex::new(stopped)),
            _recorder: None,
        }
    }
}

/// The recording thread's state.
struct Recording {
    label: String,
    reader: Arc<LiveReader>,
    dir: Arc<LiveDir>,
    stopped: Arc<Mutex<Option<String>>>,
}

impl Recording {
    /// Reread the archive after each commit, until the channel closes (the
    /// session was dropped, which stops the recorder and closes its writer
    /// first), a write fails, or the agent refuses a reconnect.
    fn run(self, mut events: tokio::sync::mpsc::Receiver<mr::Event>) {
        let label = self.label.clone();
        while let Some(event) = events.blocking_recv() {
            match event {
                mr::Event::Committed { .. } => {
                    if let Err(e) = self.reader.refresh() {
                        warn!("{label}: could not reread the live archive: {e}");
                    }
                }
                mr::Event::Dropped { reason, .. } => {
                    warn!("{label}: the stream ended ({reason}); reconnecting");
                }
                mr::Event::Reconnected { restarted, .. } => {
                    info!("{label}: the stream reconnected");
                    if restarted {
                        warn!("{label}: the agent restarted; its counters started again from zero");
                    }
                }
                mr::Event::Refused { reason, .. } => {
                    error!("{label}: the agent can no longer serve its stream: {reason}");
                    self.stop(format!(
                        "the agent can no longer serve /metrics/stream: {reason}"
                    ));
                    break;
                }
                mr::Event::LabelsNotApplied { labels, .. } => {
                    warn!(
                        "{label}: its {} label(s) arrived late; the live view shows the \
                         recording without them",
                        labels.keys().cloned().collect::<Vec<_>>().join("/")
                    );
                }
                mr::Event::Failed { error } => {
                    error!("{label}: the live recording stopped: {error}");
                    self.stop(format!("writing the live archive failed: {error}"));
                    break;
                }
                _ => {}
            }
        }
        // Release this thread's hold on the directory; the last hold
        // deletes it.
        drop(self.dir);
    }

    fn stop(&self, why: String) {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dendro_copy::fixtures::{tick, ANCHOR, SECOND};
    use crate::hindsight::buffer::HindsightBuffer;
    use metriken_query::MetricsSource;

    fn buffer(path: &Path) -> HindsightBuffer {
        let seed = crate::hindsight::buffer::Seed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: ANCHOR,
        };
        let policy = dendro::seal::SealPolicy {
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

    /// A live session against a real agent: the agent's stream handshake
    /// carries its section template, the recorder keeps it in the archive's
    /// source metadata, and the live view's built-in sections render from it,
    /// all by the time `start` returns, which is when the view's sections
    /// are built.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_live_view_renders_the_agents_section_template() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let url = crate::agent::exposition::http::serve_for_test().await;
        let client = Client::builder().http1_only().build().unwrap();
        let info = super::super::actions::fetch_agent_info(&client, &url)
            .await
            .unwrap();
        let session = LiveSession::start(&client, &url, info, BufferPool::new(64 << 20))
            .await
            .unwrap();
        let revision = format!("rezolus {}", env!("CARGO_PKG_VERSION"));

        let metadata = session.reader().file_metadata();
        let carried = ::dashboard::section_template::SectionTemplate::parse(
            &metadata[::dashboard::source_templates::KEY_DASHBOARD],
        )
        .unwrap();
        assert_eq!(carried.revision(), Some(revision.as_str()));

        let context = crate::viewer::metadata::live_dashboard_context(
            session.reader().as_ref(),
            ::dashboard::source_templates::TemplatePreference::Archive,
        );
        let template = context.template.as_ref().unwrap();
        assert_eq!(template.revision(), Some(revision.as_str()));

        // The embedded template names no release, so it is not what rendered.
        assert_eq!(
            ::dashboard::source_templates::embedded_dashboard_template().revision(),
            Some("rezolus agent")
        );
        let preferred = crate::viewer::metadata::live_dashboard_context(
            session.reader().as_ref(),
            ::dashboard::source_templates::TemplatePreference::Registry,
        );
        assert_eq!(
            preferred.template.as_ref().unwrap().revision(),
            Some("rezolus agent")
        );
    }

    /// The revision of the template the live view's built-in sections render
    /// from, and the navigation's routes.
    fn live_sections(state: &super::super::state::AppState) -> (Option<String>, Vec<String>) {
        let store = state.sections.read();
        let revision = store
            .context()
            .template
            .as_ref()
            .and_then(|t| t.revision().map(str::to_string));
        let routes = store.sections().iter().map(|s| s.route.clone()).collect();
        (revision, routes)
    }

    fn embedded_routes() -> Vec<String> {
        ::dashboard::source_templates::embedded_dashboard_template()
            .sections()
            .into_iter()
            .map(|s| s.route)
            .collect()
    }

    /// `rezolus view http://agent`: the view `init_live_mode` builds renders
    /// the built-in sections from the agent's template.
    #[test]
    fn view_of_a_live_agent_renders_the_agents_section_template() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let url = rt.block_on(crate::agent::exposition::http::serve_for_test());
        let state = super::super::init_live_mode(
            &rt,
            &url,
            &::dashboard::TemplateRegistry::empty(),
            ::dashboard::source_templates::TemplatePreference::Archive,
            BufferPool::new(64 << 20),
        );
        let (revision, routes) = live_sections(&state);
        assert_eq!(
            revision,
            Some(format!("rezolus {}", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(routes, embedded_routes());
        drop(state);
    }

    /// Connecting the page to an agent (`/api/v1/connect`): the view
    /// `connect_agent` builds renders from the agent's template.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connecting_to_a_live_agent_renders_the_agents_section_template() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let url = crate::agent::exposition::http::serve_for_test().await;
        let state = Arc::new(super::super::state::AppState::new(
            Arc::new(metriken_query::MemoryStore::builder().build()),
            ::dashboard::TemplateRegistry::empty(),
        ));
        let axum::Json(response) = super::super::actions::connect_agent(
            axum::extract::State(Arc::clone(&state)),
            axum::body::Bytes::from(url.to_string()),
        )
        .await;
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(response["status"], "success", "{response}");
        let (revision, routes) = live_sections(&state);
        assert_eq!(
            revision,
            Some(format!("rezolus {}", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(routes, embedded_routes());
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

    /// A query's answer with each series' labels sorted.
    fn answer(live: &LiveReader, query: &str) -> Vec<String> {
        let (lo, hi) = live.time_range_ns().unwrap();
        let metriken_query::QueryResult::Matrix { result } = live
            .query_range(query, lo as f64 / 1e9, hi as f64 / 1e9, 1.0)
            .unwrap()
        else {
            panic!("a range query gives a matrix");
        };
        let mut out: Vec<String> = result
            .into_iter()
            .map(|m| {
                let labels: std::collections::BTreeMap<_, _> = m.metric.into_iter().collect();
                format!("{labels:?} {:?}", m.values)
            })
            .collect();
        out.sort();
        out
    }

    const QUERIES: &[&str] = &["rate(task_ops[3s])", "mem_free"];

    /// Refreshed as rows arrive and segments seal, a reader answers as a
    /// fresh open of the archive does.
    #[test]
    fn a_refreshed_reader_answers_as_a_fresh_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.dendro");
        let mut buffer = buffer(&path);
        record(&mut buffer, 0..3);
        let pool = BufferPool::new(64 << 20);
        let live = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
        for step in 1..6u64 {
            for q in QUERIES {
                answer(&live, q);
            }
            record(&mut buffer, step * 4 - 1..step * 4 + 3);
            live.refresh().unwrap();
            let fresh = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
            for q in QUERIES {
                assert_eq!(answer(&live, q), answer(&fresh, q), "{q} at step {step}");
            }
        }
    }

    /// Two archives with the same ticks, and so segments with the same
    /// sequence numbers, row counts and spans, whose `mem_free` differs.
    fn two_archives(dir: &Path) -> (PathBuf, PathBuf) {
        let (path, other) = (dir.join("live.dendro"), dir.join("other.dendro"));
        let mut a = buffer(&path);
        record(&mut a, 0..10);
        drop(a);
        let mut b = buffer(&other);
        for i in 0..10 {
            let mut snapshot = tick(i);
            let metriken_exposition::Snapshot::V3(v3) = &mut snapshot else {
                unreachable!("the fixture is a V3 snapshot");
            };
            for group in &mut v3.groups {
                for gauge in group.gauges.iter_mut().flatten() {
                    *gauge += 5_000;
                }
            }
            b.ingest(&snapshot, ANCHOR + i * SECOND, 0).unwrap();
            b.maintain().unwrap();
        }
        b.sync().unwrap();
        drop(b);
        (path, other)
    }

    fn rename_over(from: &Path, to: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let src = format!("{}{suffix}", from.display());
            if Path::new(&src).exists() {
                std::fs::rename(&src, format!("{}{suffix}", to.display())).unwrap();
            }
        }
    }

    /// A file renamed over the path is read afresh, even where its segments
    /// have the same sequence numbers, row counts and spans as the file it
    /// replaced.
    #[test]
    fn a_file_renamed_over_the_path_is_read_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let (path, other) = two_archives(dir.path());
        let pool = BufferPool::new(64 << 20);
        let live = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
        let before = answer(&live, "mem_free");
        rename_over(&other, &path);
        live.refresh().unwrap();
        let fresh = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
        let after = answer(&live, "mem_free");
        assert_ne!(after, before, "the fixture's two files differ");
        assert_eq!(after, answer(&fresh, "mem_free"));
    }

    /// A file renamed over the path after a refresh and before the first
    /// query is not read by that reader: its tables refuse to open, and the
    /// next refresh reads the new file.
    #[test]
    fn a_file_renamed_over_the_path_after_a_refresh_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let (path, other) = two_archives(dir.path());
        let pool = BufferPool::new(64 << 20);
        let live = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
        let before = answer(&live, "mem_free");
        live.refresh().unwrap();
        rename_over(&other, &path);
        let (lo, hi) = live.time_range_ns().unwrap();
        let mixed = live.query_range("mem_free", lo as f64 / 1e9, hi as f64 / 1e9, 1.0);
        let values = match mixed {
            Ok(metriken_query::QueryResult::Matrix { result }) => {
                result.into_iter().flat_map(|m| m.values).collect()
            }
            _ => Vec::new(),
        };
        assert!(values.is_empty(), "read from the replaced file: {values:?}");

        live.refresh().unwrap();
        let fresh = LiveReader::open(&path, None, Arc::clone(&pool)).unwrap();
        let after = answer(&live, "mem_free");
        assert_ne!(after, before);
        assert_eq!(after, answer(&fresh, "mem_free"));
    }

    /// `open_file` gives the file's id when the file at the path did not
    /// change during the open, and none when it did.
    #[cfg(unix)]
    #[test]
    fn open_file_gives_an_id_only_for_an_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, other) = two_archives(dir.path());
        let ((), same) = rez::live::open_file(&path, || ());
        assert!(same.is_some());
        let ((), changed) = rez::live::open_file(&path, || rename_over(&other, &path));
        assert_eq!(changed, None);
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

    fn recording(path: &Path, stopped: &Arc<Mutex<Option<String>>>) -> Recording {
        let mut buffer = buffer(path);
        buffer.sync().unwrap();
        Recording {
            label: "test".to_string(),
            reader: Arc::new(LiveReader::open(path, None, BufferPool::new(64 << 20)).unwrap()),
            dir: Arc::new(LiveDir::create().unwrap()),
            stopped: Arc::clone(stopped),
        }
    }

    /// A reconnect the agent refuses stops the recording and records why,
    /// which the metadata route reports to the page.
    #[test]
    fn a_refused_reconnect_records_why_the_recording_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let stopped = Arc::new(Mutex::new(None));
        let rec = recording(&dir.path().join("live.dendro"), &stopped);
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.try_send(mr::Event::Refused {
            source: mr::SourceId(0),
            reason: "http://agent.test:4241/metrics/stream answered HTTP 404".to_string(),
        })
        .unwrap();
        std::thread::spawn(move || rec.run(rx)).join().unwrap();
        let why = stopped.lock().unwrap().clone().expect("a reason");
        assert!(why.contains("HTTP 404"), "{why}");
    }

    /// A session that is dropped closes the channel; that is not a failure.
    #[test]
    fn a_closed_channel_ends_the_recording_without_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let stopped = Arc::new(Mutex::new(None));
        let rec = recording(&dir.path().join("live.dendro"), &stopped);
        let (tx, rx) = tokio::sync::mpsc::channel::<mr::Event>(4);
        drop(tx);
        std::thread::spawn(move || rec.run(rx)).join().unwrap();
        assert!(stopped.lock().unwrap().is_none());
    }

    /// `refresh` finds its recording by its full label set, so a second
    /// recording whose labels include all of the first's, added after the
    /// open, does not make it ambiguous.
    #[test]
    fn a_refresh_keeps_its_recording_when_a_superset_recording_appears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grow.dendro");
        let mut writer =
            metriken_storage::ArchiveWriter::create(&path, Default::default()).unwrap();
        let first = [("source".to_string(), "rezolus".to_string())].into();
        let mut a = writer
            .add_source(first, Default::default(), ANCHOR)
            .unwrap();
        let staged = a.stage(&tick(0), ANCHOR, 0).unwrap();
        writer.commit(vec![staged]).unwrap();
        a.sync().unwrap();
        let live = LiveReader::open(&path, None, BufferPool::new(64 << 20)).unwrap();

        let second = [
            ("source".to_string(), "rezolus".to_string()),
            ("host".to_string(), "b".to_string()),
        ]
        .into();
        let mut b = writer
            .add_source(second, Default::default(), ANCHOR)
            .unwrap();
        let staged = b.stage(&tick(0), ANCHOR, 0).unwrap();
        writer.commit(vec![staged]).unwrap();
        b.sync().unwrap();

        live.refresh()
            .expect("the first recording is found by its labels");
        drop((a, b));
        writer.join().unwrap();
    }

    /// A selector that matches several recordings is refused, as the
    /// `--recording` selectors refuse one.
    #[test]
    fn a_selector_that_matches_several_recordings_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two.dendro");
        let mut writer =
            metriken_storage::ArchiveWriter::create(&path, Default::default()).unwrap();
        for host in ["a", "b"] {
            let labels = [
                ("source".to_string(), "rezolus".to_string()),
                ("host".to_string(), host.to_string()),
            ]
            .into();
            let mut source = writer
                .add_source(labels, Default::default(), ANCHOR)
                .unwrap();
            let staged = source.stage(&tick(0), ANCHOR, 0).unwrap();
            writer.commit(vec![staged]).unwrap();
            source.sync().unwrap();
        }
        writer.join().unwrap();

        let both = [("source".to_string(), "rezolus".to_string())].into();
        let err = LiveReader::open(&path, Some(both), BufferPool::new(64 << 20))
            .err()
            .expect("two recordings match");
        assert!(err.to_string().contains("2 recordings matching"), "{err}");
        let one = [("host".to_string(), "b".to_string())].into();
        assert!(LiveReader::open(&path, Some(one), BufferPool::new(64 << 20)).is_ok());
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
