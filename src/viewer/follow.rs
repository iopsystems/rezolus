//! Following an archive file that was not finalized when opened: a running
//! `rezolus hindsight` buffer, or a `rezolus record` in progress.
//!
//! An open reader's view is fixed (see `rez::live`), so file mode wraps each
//! recording it shows in a [`LiveReader`], and a [`Follow`] thread reopens
//! the archive and hands each reader its recording.
//!
//! In a dendro archive each followed recording's source carries a writer
//! heartbeat, which a running writer bumps every few seconds whether or not
//! it has rows to commit. Each reopen first reads the catalog's sources and
//! judges each followed recording's writer with a
//! [`dendro::archive::HeartbeatWatch`], then sets the wait before the next
//! reopen:
//!
//! - any writer live: [`FOLLOW_INTERVAL`], however slowly rows arrive;
//! - otherwise, every recording's writer either stopped or finalized: the
//!   writer reads as stopped when its heartbeat has not changed for
//!   [`dendro::archive::STOPPED_AFTER_INTERVALS`] heartbeat intervals of
//!   this reader's clock. It was killed, it is paused or blocked, or the
//!   file is a copy of a running archive. A paused writer that beats again
//!   reads as live again, so the follow does not end: the wait backs off as
//!   below, and returns to [`FOLLOW_INTERVAL`] once a writer beats;
//! - otherwise, some recording has no heartbeat (a `.rez`, or a dendro
//!   source whose writer did not record one): [`FOLLOW_INTERVAL`] while rows
//!   arrive, and after a reopen that finds no new row the wait doubles, up
//!   to [`MAX_WAIT`].
//!
//! The follow ends when every recording is finalized, when the file is
//! removed, or when the baseline is replaced. The page reads the `following`
//! flag in `/api/v1/mode` and the baseline's metadata.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dendro::archive::{HeartbeatWatch, WriterState};
use metriken_query::{BufferPool, MetricsSource};
use parking_lot::Mutex;
use rez::live::LiveReader;
use tracing::{info, warn};

use crate::rez_reader::RezReader;

/// How often a growing archive, or one whose writer is live, is reopened.
/// The page refreshes every 5 s, so a reopen at 2 s puts the rows it shows
/// at most about 2 s behind the writer's last commit. A reopen opens every
/// recording of the archive and parses each table's segment footers, once
/// per reopen however many recordings are shown; a reopen of a dendro
/// archive also opens its catalog read-only once to read `sources`.
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// The longest wait between two reopens of an archive that has not grown
/// and has no live writer.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// Readers of one archive being followed. Dropping it stops the thread.
pub struct Follow {
    inner: Arc<Followed>,
    /// Set on drop; the thread exits when it next wakes, which the drop
    /// causes by unparking it.
    stop: Arc<AtomicBool>,
    thread: std::thread::Thread,
    /// The thread, for a test to join after the drop.
    #[cfg(test)]
    handle: Option<std::thread::JoinHandle<()>>,
}

struct Followed {
    path: PathBuf,
    pool: Arc<BufferPool>,
    /// Whether the file is a dendro archive, whose sources carry a writer
    /// heartbeat. A `.rez` has none.
    dendro: bool,
    /// The wait while the archive grows or a writer is live.
    base: Duration,
    /// Each followed capture's id and reader.
    readers: Mutex<Vec<(String, Arc<LiveReader>)>>,
    progress: Mutex<Progress>,
    /// Set once the follow has ended, for any reason.
    done: AtomicBool,
}

struct Progress {
    /// The newest row time across the followed readers, in ns.
    newest: Option<u64>,
    /// When `newest` last advanced, or when the follow started.
    grew_at: Instant,
    /// When a reopen last read a writer as live, or when the follow started.
    live_at: Instant,
    /// The wait before the next reopen.
    wait: Duration,
    /// Whether `wait` has reached [`MAX_WAIT`] since the last growth, so
    /// reaching it, and growing again after it, are each logged once.
    capped: bool,
    /// Whether the last reopen read every writer as stopped (none live, none
    /// unknown), so entering and leaving that state are each logged once.
    stopped: bool,
    /// Whether the last reopen failed, so a run of failures is logged once.
    failing: bool,
    /// Whether the last read of a dendro archive's sources failed, logged
    /// once per run of failures as `failing` is.
    catalog_failing: bool,
    /// One heartbeat watch per followed recording, by its label set.
    watches: BTreeMap<BTreeMap<String, String>, HeartbeatWatch>,
}

/// One followed recording's writer, as a reopen reads it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writer {
    Live,
    Stopped,
    Complete,
    /// No heartbeat to judge by.
    Unknown,
}

#[cfg(test)]
thread_local! {
    /// Set by [`start_parked`]: a follow started on this thread keeps its
    /// thread parked until it is dropped, so a test drives every reopen.
    static START_PARKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with every follow it starts on this thread parked: the thread
/// never reopens the archive, and the test drives every reopen through
/// [`Follow::refresh_at`].
#[cfg(test)]
pub(crate) fn start_parked<T>(f: impl FnOnce() -> T) -> T {
    START_PARKED.with(|p| p.set(true));
    let out = f();
    START_PARKED.with(|p| p.set(false));
    out
}

impl Follow {
    /// Start reopening the archive behind `readers` on a thread of its own,
    /// since a reopen does blocking IO, waiting `interval` between reopens
    /// while it grows or a writer is live. Each entry is a capture id and the
    /// reader serving it. The readers must all read one archive, and no two
    /// of them may have the same label set (see [`LiveReader::from_reader`]).
    pub fn start(
        readers: Vec<(String, Arc<LiveReader>)>,
        pool: Arc<BufferPool>,
        interval: Duration,
    ) -> std::io::Result<Self> {
        debug_assert!(!readers.is_empty(), "a follow needs a reader");
        let path = readers
            .first()
            .map(|(_, r)| r.path().to_path_buf())
            .unwrap_or_default();
        let newest = newest_row(&readers);
        let dendro = metriken_archive::DendroCatalog::is_archive(&path).unwrap_or(false);
        let started = Instant::now();
        let inner = Arc::new(Followed {
            path,
            pool,
            dendro,
            base: interval,
            readers: Mutex::new(readers),
            progress: Mutex::new(Progress {
                newest,
                grew_at: started,
                live_at: started,
                wait: interval,
                capped: false,
                stopped: false,
                failing: false,
                catalog_failing: false,
                watches: BTreeMap::new(),
            }),
            done: AtomicBool::new(false),
        });
        #[cfg(test)]
        let parked = START_PARKED.with(|p| p.get());
        #[cfg(not(test))]
        let parked = false;
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_inner, thread_stop) = (Arc::clone(&inner), Arc::clone(&stop));
        let handle = std::thread::Builder::new()
            .name("rezolus-follow".to_string())
            .spawn(move || {
                if parked {
                    while !thread_stop.load(Ordering::Acquire) {
                        std::thread::park();
                    }
                    return;
                }
                loop {
                    let deadline = Instant::now() + thread_inner.wait();
                    // `park_timeout` can return early, spuriously or from
                    // the drop's unpark; only the deadline or a stop ends
                    // the wait.
                    loop {
                        let now = Instant::now();
                        if thread_stop.load(Ordering::Acquire) || now >= deadline {
                            break;
                        }
                        std::thread::park_timeout(deadline - now);
                    }
                    // A stop comes from the drop, which marks the follow
                    // ended itself.
                    if thread_stop.load(Ordering::Acquire) {
                        return;
                    }
                    if !thread_inner.step(Instant::now()) {
                        thread_inner.done.store(true, Ordering::Release);
                        return;
                    }
                }
            })?;
        Ok(Self {
            inner,
            stop,
            thread: handle.thread().clone(),
            #[cfg(test)]
            handle: Some(handle),
        })
    }

    /// Whether the archive is still being followed.
    pub fn active(&self) -> bool {
        !self.inner.done.load(Ordering::Acquire)
    }

    /// Stop reopening the capture `id`: its slot was detached or replaced.
    /// When no capture is left, the follow ends.
    pub fn drop_capture(&self, id: &str) {
        let mut readers = self.inner.readers.lock();
        readers.retain(|(c, _)| c != id);
        if readers.is_empty() {
            self.inner.done.store(true, Ordering::Release);
        }
    }

    /// Drop the follow and wait up to `within` for its thread to exit.
    /// Returns whether it did.
    #[cfg(test)]
    pub fn drop_and_join(mut self, within: Duration) -> bool {
        let handle = self.handle.take().expect("the thread is joined once");
        drop(self);
        let deadline = Instant::now() + within;
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.is_finished() && handle.join().is_ok()
    }

    /// The capture ids still followed.
    #[cfg(test)]
    pub fn captures(&self) -> Vec<String> {
        self.inner
            .readers
            .lock()
            .iter()
            .map(|(c, _)| c.clone())
            .collect()
    }

    /// Reopen the archive now, as the thread does after each wait. Returns
    /// whether to keep following; when not, the follow ends.
    #[cfg(test)]
    pub fn refresh_now(&self) -> bool {
        self.refresh_at(Instant::now())
    }

    /// [`refresh_now`](Self::refresh_now) as if the clock read `now`.
    #[cfg(test)]
    pub fn refresh_at(&self, now: Instant) -> bool {
        let more = self.inner.step(now);
        if !more {
            self.inner.done.store(true, Ordering::Release);
        }
        more
    }

    /// The wait before the next reopen.
    #[cfg(test)]
    pub fn wait(&self) -> Duration {
        self.inner.wait()
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.inner.done.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

impl Followed {
    fn wait(&self) -> Duration {
        self.progress.lock().wait
    }

    /// Read the writers' heartbeats, reopen the archive and hand each reader
    /// its recording, then set the wait before the next reopen. `now` is the
    /// time of this reopen. A reader whose recording cannot be read keeps
    /// its previous view, and a run of failures is logged once. Returns
    /// whether to keep following.
    fn step(&self, now: Instant) -> bool {
        if self.done.load(Ordering::Acquire) {
            return false;
        }
        let path = self.path.display();
        if !self.path.exists() {
            info!("{path} was removed; no longer following it");
            return false;
        }
        let readers = self.readers.lock().clone();
        if readers.is_empty() {
            return false;
        }
        let mut progress = self.progress.lock();

        // The catalog first: a source it reads as finalized is finalized in
        // the open that follows, so the two cannot disagree that way round.
        let writers = self.writers(&readers, &mut progress, now);

        let (opened, file) = rez::live::open_file(&self.path, || {
            quiet(|| RezReader::open_recordings(&self.path, Arc::clone(&self.pool)))
        });
        let failure = match opened {
            Ok(mut recordings) => {
                let mut missing = None;
                for (_, reader) in &readers {
                    match recordings.iter().position(|(l, _)| l == reader.labels()) {
                        Some(at) => reader.replace(recordings.swap_remove(at).1, file),
                        None => {
                            missing = Some(format!("no recording labelled {:?}", reader.labels()))
                        }
                    }
                }
                missing
            }
            Err(e) => Some(e.to_string()),
        };
        match failure {
            Some(e) if !progress.failing => {
                warn!("{path}: could not reread the archive: {e}");
                progress.failing = true;
            }
            Some(_) => {}
            None => progress.failing = false,
        }

        if readers.iter().all(|(_, r)| r.complete()) {
            info!("{path} was finalized; no longer following it");
            return false;
        }

        let newest = newest_row(&readers);
        if writers.contains(&Writer::Live) {
            if progress.stopped {
                info!(
                    "{path}: its writer is beating again; checking it every {}",
                    humantime::format_duration(self.base)
                );
                progress.stopped = false;
            }
            progress.live_at = now;
            progress.wait = self.base;
            progress.capped = false;
            if newest > progress.newest {
                progress.newest = newest;
                progress.grew_at = now;
            }
            return true;
        }

        let stopped = writers.contains(&Writer::Stopped) && !writers.contains(&Writer::Unknown);
        if stopped && !progress.stopped {
            progress.stopped = true;
            // From the last reopen that read a writer as live, so it is the
            // silence this reader saw, to within one wait.
            let quiet_for = now.saturating_duration_since(progress.live_at);
            info!(
                "{path}: its writer has not beaten in {} (killed, paused, or the file is a \
                 copy); checking it every {}",
                humantime::format_duration(Duration::from_secs(quiet_for.as_secs())),
                humantime::format_duration(MAX_WAIT)
            );
        }
        self.back_off(&mut progress, newest, now, !stopped);
        true
    }

    /// Double the wait after a reopen that found no new row, up to
    /// [`MAX_WAIT`], or reset it after one that found one. With `log`,
    /// reaching the cap and growing again after it are each logged once.
    fn back_off(&self, progress: &mut Progress, newest: Option<u64>, now: Instant, log: bool) {
        let path = self.path.display();
        if newest > progress.newest {
            if progress.capped && log {
                info!(
                    "{path} grew again; checking it every {}",
                    humantime::format_duration(self.base)
                );
            }
            progress.newest = newest;
            progress.grew_at = now;
            progress.wait = self.base;
            progress.capped = false;
            return;
        }
        progress.wait = progress.wait.saturating_mul(2).min(MAX_WAIT);
        if progress.wait == MAX_WAIT && !progress.capped {
            progress.capped = true;
            if log {
                let idle = now.saturating_duration_since(progress.grew_at);
                info!(
                    "{path} has not grown in {}; checking it every {}",
                    humantime::format_duration(Duration::from_secs(idle.as_secs())),
                    humantime::format_duration(MAX_WAIT)
                );
            }
        }
    }

    /// Each followed recording's writer, judged from its heartbeat. Reads
    /// the catalog's sources once; a recording is matched to the source with
    /// its exact label set. Every recording of a `.rez`, of a dendro archive
    /// whose sources cannot be read, or without a matching source is
    /// [`Unknown`](Writer::Unknown). A failed read is logged once per run of
    /// failures.
    fn writers(
        &self,
        readers: &[(String, Arc<LiveReader>)],
        progress: &mut Progress,
        now: Instant,
    ) -> Vec<Writer> {
        let sources = if self.dendro {
            match dendro::archive::Archive::open(&self.path).and_then(|a| a.read_sources()) {
                Ok(sources) => {
                    progress.catalog_failing = false;
                    Some(sources)
                }
                Err(e) => {
                    if !progress.catalog_failing {
                        warn!(
                            "{}: could not read its writers' heartbeats: {e}",
                            self.path.display()
                        );
                        progress.catalog_failing = true;
                    }
                    None
                }
            }
        } else {
            None
        };
        readers
            .iter()
            .map(|(_, reader)| {
                let source = sources
                    .as_ref()
                    .and_then(|s| s.iter().find(|s| s.meta.labels == *reader.labels()));
                let Some(source) = source else {
                    return Writer::Unknown;
                };
                let state = progress
                    .watches
                    .entry(reader.labels().clone())
                    .or_default()
                    .observe(source, now);
                match state {
                    WriterState::Live => Writer::Live,
                    WriterState::Stopped => Writer::Stopped,
                    WriterState::Complete => Writer::Complete,
                    WriterState::Unknown => Writer::Unknown,
                    _ => Writer::Unknown,
                }
            })
            .collect()
    }
}

/// The newest row time across `readers`, in ns.
fn newest_row(readers: &[(String, Arc<LiveReader>)]) -> Option<u64> {
    readers
        .iter()
        .filter_map(|(_, r)| r.time_range_ns().map(|(_, end)| end))
        .max()
}

/// Run `f` with every tracing event on this thread dropped: every reopen of
/// an unfinalized archive would otherwise log that it was not finalized.
fn quiet<T>(f: impl FnOnce() -> T) -> T {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), f)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use metriken_archive::{ArchiveWriter, SourceRecorder, WriterConfig};

    use metriken_query::MetricsSource;
    use rez::live::LiveReader;

    use super::FOLLOW_INTERVAL;
    use crate::rez_reader::RezReader;

    use crate::dendro_copy::fixtures::{recorded, tick, ANCHOR, SECOND};
    use crate::viewer::state::AppState;

    fn writer(path: &Path) -> (ArchiveWriter, SourceRecorder) {
        let config = WriterConfig {
            seal: dendro::seal::SealPolicy {
                max_rows: 4,
                ..dendro::seal::SealPolicy::default()
            },
            ..WriterConfig::default()
        };
        let mut writer = ArchiveWriter::create(path, config).unwrap();
        let labels: BTreeMap<String, String> = [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect();
        let source = writer.add_source(labels, BTreeMap::new(), ANCHOR).unwrap();
        (writer, source)
    }

    fn write(writer: &mut ArchiveWriter, source: &mut SourceRecorder, ticks: std::ops::Range<u64>) {
        for i in ticks {
            let staged = source.stage(&tick(i), ANCHOR + i * SECOND, 0).unwrap();
            writer.commit(vec![staged]).unwrap();
            source.maybe_seal().unwrap();
        }
        source.sync().unwrap();
    }

    /// Open `path` the way `rezolus view <path>` does. The follow's thread
    /// starts parked, so every reopen is the test's.
    fn view(path: &Path) -> AppState {
        let matches = crate::viewer::command().get_matches_from(["view", path.to_str().unwrap()]);
        let config = crate::viewer::Config::try_from(matches).unwrap();
        super::start_parked(|| {
            crate::viewer::init_file_mode(
                &config,
                path,
                &::dashboard::TemplateRegistry::empty(),
                metriken_query::BufferPool::new(64 << 20),
            )
        })
    }

    /// `end_time` from the `/api/v1/sections` payload, in ms.
    fn end_ms(state: &AppState) -> u64 {
        state.sections_metadata()["end_time"].as_u64().unwrap()
    }

    /// An archive opened while its writer appends is followed: after a
    /// refresh the metadata's end time includes the rows written since the
    /// open. Once the writer finalizes, the next refresh ends the follow.
    #[test]
    fn a_file_being_written_is_followed_until_it_is_finalized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let (mut writer, mut source) = writer(&path);
        write(&mut writer, &mut source, 0..3);

        let state = view(&path);
        assert!(state.following(), "an unfinalized archive is followed");
        assert_eq!(end_ms(&state), (ANCHOR + 2 * SECOND) / 1_000_000);

        write(&mut writer, &mut source, 3..10);
        assert!(state.follow.lock().as_ref().unwrap().refresh_now());
        assert_eq!(end_ms(&state), (ANCHOR + 9 * SECOND) / 1_000_000);
        // The PromQL path reads the new rows too, not just the range.
        let at = (ANCHOR + 9 * SECOND) as f64 / 1e9;
        let metriken_query::QueryResult::Vector { result } =
            state.baseline_data().query("mem_free", Some(at)).unwrap()
        else {
            panic!("an instant query gives a vector");
        };
        assert_eq!(result[0].value.1, 991.0);

        source.finalize((ANCHOR + 9 * SECOND, 0)).unwrap();
        writer.join().unwrap();
        assert!(
            !state.follow.lock().as_ref().unwrap().refresh_now(),
            "a finalized archive ends the follow"
        );
        assert!(!state.following());
    }

    /// Hindsight removes its buffer on exit without finalizing it. The
    /// follow ends, and the readers keep the range they last read. (A table
    /// no query had opened before the removal cannot be read after it, as
    /// for any archive file removed while a viewer has it open.)
    #[test]
    fn a_removed_file_ends_the_follow_and_keeps_the_last_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        recorded(&path, 5, false);
        let state = view(&path);
        assert!(state.following());

        std::fs::remove_file(&path).unwrap();
        assert!(!state.follow.lock().as_ref().unwrap().refresh_now());
        assert!(!state.following());
        assert_eq!(end_ms(&state), (ANCHOR + 4 * SECOND) / 1_000_000);
    }

    /// A follow of the only recording at `path`, started as file mode starts
    /// one but waiting `interval`, with its thread running.
    fn follow_running(path: &Path, interval: Duration) -> (super::Follow, Arc<LiveReader>) {
        let pool = metriken_query::BufferPool::new(64 << 20);
        let (labels, reader) = RezReader::open_recordings(path, Arc::clone(&pool))
            .unwrap()
            .remove(0);
        let live = Arc::new(LiveReader::from_reader(
            path,
            labels,
            reader,
            Arc::clone(&pool),
        ));
        let readers = vec![("baseline".to_string(), Arc::clone(&live))];
        (super::Follow::start(readers, pool, interval).unwrap(), live)
    }

    /// The follow's own thread reopens the archive: rows a writer commits
    /// after the open are read without a test calling `refresh_at`.
    #[test]
    fn the_follow_thread_reads_new_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threaded.dendro");
        let (mut writer, mut source) = writer(&path);
        write(&mut writer, &mut source, 0..3);
        let (follow, live) = follow_running(&path, Duration::from_millis(20));
        assert_eq!(live.time_range_ns().unwrap().1, ANCHOR + 2 * SECOND);

        write(&mut writer, &mut source, 3..10);
        let deadline = Instant::now() + Duration::from_secs(5);
        while live.time_range_ns().unwrap().1 != ANCHOR + 9 * SECOND {
            assert!(
                Instant::now() < deadline,
                "the thread did not read the new rows within 5 s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(follow.active());
        drop(follow);
        drop((source, writer));
    }

    /// Dropping a follow wakes its thread from a long wait, and it exits.
    #[test]
    fn dropping_the_follow_ends_its_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idle.dendro");
        recorded(&path, 5, false);
        let (follow, _live) = follow_running(&path, Duration::from_secs(60));
        // Let the thread reach its 60 s wait; a drop before the thread first
        // runs would end it without testing the wake-up.
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            follow.drop_and_join(Duration::from_secs(5)),
            "the thread exits on drop, not after its 60 s wait"
        );
    }

    /// Commit one row at `minute` minutes past the anchor.
    fn commit_minute(writer: &mut ArchiveWriter, source: &mut SourceRecorder, minute: u64) {
        let staged = source
            .stage(&tick(minute), ANCHOR + minute * 60 * SECOND, 0)
            .unwrap();
        writer.commit(vec![staged]).unwrap();
        source.sync().unwrap();
    }

    /// Bump every source's heartbeat, as a running writer does every 5 s. The
    /// tests drive the reader's clock through `refresh_at`, faster than the
    /// writer's own timer runs, so they beat for it.
    fn beat(path: &Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.busy_timeout(Duration::from_secs(10)).unwrap();
        conn.execute(
            "UPDATE sources SET heartbeat = COALESCE(heartbeat, 0) + 1",
            [],
        )
        .unwrap();
    }

    /// The heartbeat interval a dendro writer records: 5 s.
    const BEAT: Duration = Duration::from_secs(5);

    /// A live writer that commits a row once a minute (hindsight at a 60 s
    /// interval) is reopened every 2 s throughout, and each row is read on
    /// the first reopen after it is committed.
    #[test]
    fn a_live_writer_that_commits_once_a_minute_is_reopened_every_two_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slow.dendro");
        let (mut writer, mut source) = writer(&path);
        commit_minute(&mut writer, &mut source, 0);
        let state = view(&path);
        let guard = state.follow.lock();
        let follow = guard.as_ref().unwrap();

        let start = Instant::now();
        let mut clock = start;
        let mut since_beat = Duration::ZERO;
        for minute in 1..=3u64 {
            let commit_at = start + Duration::from_secs(60 * minute);
            while clock + follow.wait() < commit_at {
                clock += follow.wait();
                since_beat += follow.wait();
                if since_beat >= BEAT {
                    beat(&path);
                    since_beat = Duration::ZERO;
                }
                assert!(follow.refresh_at(clock), "followed while idle");
                assert_eq!(follow.wait(), FOLLOW_INTERVAL, "no backoff while live");
            }
            commit_minute(&mut writer, &mut source, minute);
            clock += follow.wait();
            assert!(follow.refresh_at(clock));
            assert_eq!(follow.wait(), FOLLOW_INTERVAL);
            assert_eq!(end_ms(&state), (ANCHOR + minute * 60 * SECOND) / 1_000_000);
        }
        drop(guard);
        assert!(state.following());
    }

    /// A writer whose heartbeat stays fixed (killed, or paused: Ctrl-Z, a
    /// stalled disk) reads as stopped after three heartbeat intervals. The
    /// follow does not end: the wait backs off to 60 s, and returns to 2 s
    /// once the writer beats again.
    #[test]
    fn a_stopped_writer_is_checked_less_often_until_it_beats_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paused.dendro");
        recorded(&path, 5, false);
        let state = view(&path);
        let guard = state.follow.lock();
        let follow = guard.as_ref().unwrap();

        let start = Instant::now();
        assert!(follow.refresh_at(start), "first look: not yet judged");
        assert!(follow.refresh_at(start + BEAT * 3 - Duration::from_millis(1)));
        assert_eq!(follow.wait(), FOLLOW_INTERVAL, "live until three intervals");
        let mut clock = start + BEAT * 3;
        for expected in [4, 8, 16, 32, 60, 60] {
            assert!(follow.refresh_at(clock), "a stopped writer stays followed");
            assert_eq!(follow.wait(), Duration::from_secs(expected));
            clock += follow.wait();
        }

        beat(&path);
        assert!(follow.refresh_at(clock));
        assert_eq!(follow.wait(), FOLLOW_INTERVAL, "a beat ends the backoff");
        drop(guard);
        assert!(state.following());
    }

    /// A copy of a running archive carries the heartbeat columns, and no
    /// writer bumps the copy. After three intervals it reads as stopped and
    /// backs off, while the original, whose writer beats, stays at 2 s.
    #[test]
    fn a_copy_of_a_running_archive_backs_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("running.dendro");
        let (mut writer, mut source) = writer(&path);
        commit_minute(&mut writer, &mut source, 0);
        let copy = dir.path().join("copy.dendro");
        let src = dendro::archive::Archive::open(&path).unwrap();
        crate::dendro_copy::copy(&src, &copy, &dendro::rewrite::CopySpec::everything()).unwrap();
        drop(src);

        let original = view(&path);
        let copied = view(&copy);
        let start = Instant::now();
        for (state, beats) in [(&original, true), (&copied, false)] {
            let guard = state.follow.lock();
            let follow = guard.as_ref().unwrap();
            assert!(follow.refresh_at(start));
            if beats {
                beat(&path);
            }
            assert!(follow.refresh_at(start + BEAT * 3));
            let expected = if beats {
                FOLLOW_INTERVAL
            } else {
                FOLLOW_INTERVAL * 2
            };
            assert_eq!(follow.wait(), expected);
        }
        assert!(original.following());
        assert!(copied.following());
        drop((source, writer));
    }

    /// A `.rez` has no heartbeat, so its writer's state is unknown. After a
    /// reopen that finds no new row the wait doubles, from 2 s up to 60 s,
    /// a reopen that finds one resets it, and the follow does not end.
    #[test]
    fn a_rez_backs_off_while_idle_and_resets_on_growth() {
        use crate::hindsight::buffer::HindsightBuffer;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let seed = crate::recorder::rez_v3_writer::ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: ANCHOR,
        };
        let mut buffer = HindsightBuffer::create(
            &path,
            seed,
            Duration::MAX,
            crate::recorder::seal_policy::SealPolicy::default(),
        )
        .unwrap();
        let mut record = |i: u64| {
            buffer
                .ingest(&tick(i), ANCHOR + i * 60 * SECOND, 0)
                .unwrap();
            buffer.maintain().unwrap();
            buffer.sync().unwrap();
        };
        record(0);
        let state = view(&path);
        let guard = state.follow.lock();
        let follow = guard.as_ref().unwrap();
        assert_eq!(follow.wait(), FOLLOW_INTERVAL);

        let mut clock = Instant::now();
        for expected in [4, 8, 16, 32, 60, 60, 60] {
            clock += follow.wait();
            assert!(follow.refresh_at(clock), "a .rez is never judged stopped");
            assert_eq!(follow.wait(), Duration::from_secs(expected));
        }
        record(1);
        clock += follow.wait();
        assert!(follow.refresh_at(clock));
        assert_eq!(follow.wait(), FOLLOW_INTERVAL);
        assert_eq!(end_ms(&state), (ANCHOR + 60 * SECOND) / 1_000_000);
        clock += follow.wait();
        assert!(follow.refresh_at(clock));
        assert_eq!(follow.wait(), Duration::from_secs(4));
    }

    /// Two recordings fill the A/B slots. Each is followed, from one reopen
    /// per tick, and detaching the experiment stops reopening it.
    #[test]
    fn both_slots_of_an_ab_archive_are_followed_until_detached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.dendro");
        let mut writer = ArchiveWriter::create(&path, WriterConfig::default()).unwrap();
        let mut sources: Vec<SourceRecorder> = ["a", "b"]
            .into_iter()
            .map(|host| {
                let labels = [
                    ("source".to_string(), "rezolus".to_string()),
                    ("host".to_string(), host.to_string()),
                ]
                .into();
                writer.add_source(labels, BTreeMap::new(), ANCHOR).unwrap()
            })
            .collect();
        let mut write_both = |ticks: std::ops::Range<u64>| {
            for i in ticks {
                let staged = sources
                    .iter_mut()
                    .map(|s| s.stage(&tick(i), ANCHOR + i * SECOND, 0).unwrap())
                    .collect();
                writer.commit(staged).unwrap();
            }
            for s in &mut sources {
                s.sync().unwrap();
            }
        };
        write_both(0..3);
        let state = view(&path);
        let follow_ids = || state.follow.lock().as_ref().unwrap().captures();
        assert_eq!(follow_ids(), ["baseline", "experiment"]);

        write_both(3..6);
        assert!(state.follow.lock().as_ref().unwrap().refresh_now());
        let end = |id| {
            state
                .captures
                .get_by_id(id)
                .unwrap()
                .time_range_ns()
                .unwrap()
                .1
        };
        assert_eq!(end("baseline"), ANCHOR + 5 * SECOND);
        assert_eq!(end("experiment"), ANCHOR + 5 * SECOND);

        state.unfollow_capture("experiment");
        assert_eq!(follow_ids(), ["baseline"]);
        assert!(state.following());
    }

    /// A reopen finds each recording by its label set, so an archive whose
    /// shown recordings share one is not followed.
    #[test]
    fn recordings_that_share_a_label_set_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twins.dendro");
        let mut writer = ArchiveWriter::create(&path, WriterConfig::default()).unwrap();
        for _ in 0..2 {
            let labels = [("source".to_string(), "rezolus".to_string())].into();
            let mut source = writer.add_source(labels, BTreeMap::new(), ANCHOR).unwrap();
            let staged = source.stage(&tick(0), ANCHOR, 0).unwrap();
            writer.commit(vec![staged]).unwrap();
            source.sync().unwrap();
        }
        let state = view(&path);
        assert!(!state.following());
        assert!(state.follow.lock().is_none());
        drop(writer);
    }

    /// A finalized archive is opened once and never reopened.
    #[test]
    fn a_finalized_archive_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("done.dendro");
        recorded(&path, 5, true);
        let state = view(&path);
        assert!(!state.following());
        assert!(state.follow.lock().is_none());
    }

    /// A hindsight buffer evicts rows older than its lookback, so the start
    /// of a followed archive's range moves forward on refresh.
    #[test]
    fn a_followed_buffer_start_moves_forward_as_rows_are_evicted() {
        use crate::hindsight::buffer::HindsightBuffer;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hindsight.dendro");
        let seed = crate::recorder::rez_v3_writer::ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: ANCHOR,
        };
        let policy = crate::recorder::seal_policy::SealPolicy {
            max_rows: 2,
            ..Default::default()
        };
        let mut buffer =
            HindsightBuffer::create_dendro(&path, seed, std::time::Duration::from_secs(4), policy)
                .unwrap();
        let mut record = |ticks: std::ops::Range<u64>| {
            for i in ticks {
                buffer.ingest(&tick(i), ANCHOR + i * SECOND, 0).unwrap();
                buffer.maintain().unwrap();
            }
            buffer.sync().unwrap();
        };
        record(0..3);
        let state = view(&path);
        let start = |s: &AppState| s.sections_metadata()["start_time"].as_u64().unwrap();
        assert_eq!(start(&state), ANCHOR / 1_000_000);

        record(3..20);
        state.follow.lock().as_ref().unwrap().refresh_now();
        assert!(
            start(&state) > (ANCHOR + 10 * SECOND) / 1_000_000,
            "the evicted rows are gone from the range: start {}",
            start(&state)
        );
        assert_eq!(end_ms(&state), (ANCHOR + 19 * SECOND) / 1_000_000);
    }
}
