//! Following an archive file that was not finalized when opened: a running
//! `rezolus hindsight` buffer, or a `rezolus record` in progress.
//!
//! An open reader's view is fixed (see `rez::live`), so file mode wraps each
//! recording it shows in a [`LiveReader`], and a [`Follow`] thread reopens
//! the archive every [`FOLLOW_INTERVAL`] and hands each reader its recording.
//! The follow ends when every recording is finalized, when the file is
//! removed, or when the newest row has not advanced for the stall bound (see
//! [`stall_bound`]): a file whose writer was killed, or a copy of a running
//! archive, is never finalized. The page reads the `following` flag in
//! `/api/v1/mode` and the baseline's metadata.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use metriken_query::{BufferPool, MetricsSource};
use parking_lot::Mutex;
use rez::live::LiveReader;
use tracing::{info, warn};

use crate::rez_reader::RezReader;

/// How often a followed archive is reopened. The page refreshes every 5 s,
/// so a reopen at 2 s puts the rows it shows at most about 2 s behind the
/// writer's last commit. A reopen opens every recording of the archive and
/// parses each table's segment footers, once per tick however many
/// recordings are shown.
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// The shortest time without a new row after which a follow ends.
const STALL_FLOOR: Duration = Duration::from_secs(30);

/// How many of the archive's sampling intervals without a new row end a
/// follow, when that is longer than [`STALL_FLOOR`].
const STALL_INTERVALS: f64 = 10.0;

/// Readers of one archive being followed. Dropping it stops the thread
/// within one interval.
pub struct Follow {
    inner: Arc<Followed>,
    /// Set on drop; the thread exits at its next wake-up.
    stop: Arc<AtomicBool>,
}

struct Followed {
    path: PathBuf,
    pool: Arc<BufferPool>,
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
    since: Instant,
    /// Whether the last reopen failed, so a run of failures is logged once.
    failing: bool,
    /// Replaces [`stall_bound`] (tests).
    stall_override: Option<Duration>,
}

impl Follow {
    /// Start reopening the archive behind `readers` every `interval` on a
    /// thread of its own, since a reopen does blocking IO. Each entry is a
    /// capture id and the reader serving it. The readers must all read one
    /// archive, and no two of them may have the same label set (see
    /// [`LiveReader::from_reader`]).
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
        let inner = Arc::new(Followed {
            path,
            pool,
            readers: Mutex::new(readers),
            progress: Mutex::new(Progress {
                newest,
                since: Instant::now(),
                failing: false,
                stall_override: None,
            }),
            done: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_inner, thread_stop) = (Arc::clone(&inner), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("rezolus-follow".to_string())
            .spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    if thread_stop.load(Ordering::Acquire) || !thread_inner.step() {
                        break;
                    }
                }
                thread_inner.done.store(true, Ordering::Release);
            })?;
        Ok(Self { inner, stop })
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

    /// Reopen the archive now, as the thread does each interval. Returns
    /// whether to keep following; when not, the follow ends.
    #[cfg(test)]
    pub fn refresh_now(&self) -> bool {
        let more = self.inner.step();
        if !more {
            self.inner.done.store(true, Ordering::Release);
        }
        more
    }

    /// Use `bound` in place of [`stall_bound`].
    #[cfg(test)]
    pub fn set_stall_bound(&self, bound: Duration) {
        self.inner.progress.lock().stall_override = Some(bound);
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.inner.done.store(true, Ordering::Release);
    }
}

impl Followed {
    /// Reopen the archive once and hand each reader its recording. A reader
    /// whose recording cannot be read keeps its previous view, and a run of
    /// failures is logged once. Returns whether to keep following.
    fn step(&self) -> bool {
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

        let opened = quiet(|| RezReader::open_recordings(&self.path, Arc::clone(&self.pool)));
        let mut progress = self.progress.lock();
        let failure = match opened {
            Ok(mut recordings) => {
                let mut missing = None;
                for (_, reader) in &readers {
                    match recordings.iter().position(|(l, _)| l == reader.labels()) {
                        Some(at) => reader.replace(recordings.swap_remove(at).1),
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
        if newest > progress.newest {
            progress.newest = newest;
            progress.since = Instant::now();
            return true;
        }
        let idle = progress.since.elapsed();
        let bound = progress
            .stall_override
            .unwrap_or_else(|| stall_bound(&readers));
        if idle >= bound {
            info!(
                "{path} has not grown in {}; no longer following it (its writer is not \
                 running, or the file is a copy)",
                humantime::format_duration(Duration::from_secs(idle.as_secs()))
            );
            return false;
        }
        true
    }
}

/// How long a followed archive may go without a new row before the follow
/// ends: ten of its sampling intervals, and at least [`STALL_FLOOR`]. The
/// interval is the slowest of the readers' measured intervals.
fn stall_bound(readers: &[(String, Arc<LiveReader>)]) -> Duration {
    let slowest = readers
        .iter()
        .map(|(_, r)| r.interval())
        .filter(|i| i.is_finite() && *i > 0.0)
        .fold(0.0, f64::max);
    STALL_FLOOR.max(Duration::try_from_secs_f64(slowest * STALL_INTERVALS).unwrap_or(Duration::MAX))
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
    use std::time::Duration;

    use metriken_archive::{ArchiveWriter, SourceRecorder, WriterConfig};

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

    /// Open `path` the way `rezolus view <path>` does.
    fn view(path: &Path) -> AppState {
        let matches = crate::viewer::command().get_matches_from(["view", path.to_str().unwrap()]);
        let config = crate::viewer::Config::try_from(matches).unwrap();
        crate::viewer::init_file_mode(
            &config,
            path,
            &::dashboard::TemplateRegistry::empty(),
            metriken_query::BufferPool::new(64 << 20),
        )
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

    /// An unfinalized archive with no writer (a killed hindsight's buffer, a
    /// copy of a running archive) never grows. The follow ends once no row
    /// has arrived for the stall bound.
    #[test]
    fn a_file_with_no_writer_stops_being_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copy.dendro");
        recorded(&path, 5, false);
        let state = view(&path);
        assert!(state.following(), "it is not finalized, so it is followed");
        let follow = state.follow.lock();
        let follow = follow.as_ref().unwrap();
        assert!(follow.refresh_now(), "within the default bound of 30 s");
        follow.set_stall_bound(Duration::ZERO);
        assert!(
            !follow.refresh_now(),
            "a file that does not grow stops being followed"
        );
        assert!(!follow.active());
    }

    /// The default bound is ten of the archive's intervals, at least 30 s.
    #[test]
    fn the_stall_bound_is_ten_intervals_and_at_least_thirty_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-second.dendro");
        recorded(&path, 5, false);
        let state = view(&path);
        let follow = state.follow.lock();
        let readers = follow.as_ref().unwrap().inner.readers.lock().clone();
        assert_eq!(super::stall_bound(&readers), Duration::from_secs(30));
        assert_eq!(super::stall_bound(&[]), Duration::from_secs(30));

        // Rows 10 s apart: ten intervals is 100 s.
        let path = dir.path().join("ten-second.dendro");
        let (mut writer, mut source) = writer(&path);
        for i in 0..5 {
            let staged = source.stage(&tick(i), ANCHOR + i * 10 * SECOND, 0).unwrap();
            writer.commit(vec![staged]).unwrap();
        }
        source.sync().unwrap();
        let slow = view(&path);
        let follow = slow.follow.lock();
        let readers = follow.as_ref().unwrap().inner.readers.lock().clone();
        assert_eq!(super::stall_bound(&readers), Duration::from_secs(100));
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
