//! Following an archive file that is still being written: a running
//! `rezolus hindsight` buffer, or a `rezolus record` in progress.
//!
//! An open reader's view is fixed (see `rez::live`), so file mode wraps each
//! recording it shows in a [`LiveReader`] and a [`Follow`] thread refreshes
//! them every [`FOLLOW_INTERVAL`] until every one of them is finalized or the
//! file is removed. The
//! page learns that the view advances from the `following` flag in
//! `/api/v1/mode` and the baseline's metadata.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rez::live::LiveReader;
use tracing::{info, warn};

/// How often a followed archive is reopened. The page refreshes every 5 s,
/// so a reopen at 2 s puts the rows it shows at most about 2 s behind the
/// writer's last commit. A reopen reads the catalog; table footers are read
/// on a table's first query.
pub const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);

/// Readers of one archive being followed. Dropping it stops the thread
/// within one interval.
pub struct Follow {
    readers: Arc<Vec<Arc<LiveReader>>>,
    path: PathBuf,
    /// Set on drop; the thread exits at its next wake-up.
    stop: Arc<AtomicBool>,
    /// Set once every reader is finalized, or the follow was stopped.
    done: Arc<AtomicBool>,
}

impl Follow {
    /// Start refreshing `readers` every `interval` on a thread of their own,
    /// since a reopen does blocking IO. `readers` must not be empty.
    pub fn start(readers: Vec<Arc<LiveReader>>, interval: Duration) -> std::io::Result<Self> {
        let path = readers
            .first()
            .map(|r| r.path().to_path_buf())
            .unwrap_or_default();
        let follow = Self {
            readers: Arc::new(readers),
            path,
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
        };
        let readers = Arc::clone(&follow.readers);
        let path = follow.path.clone();
        let stop = Arc::clone(&follow.stop);
        let done = Arc::clone(&follow.done);
        std::thread::Builder::new()
            .name("rezolus-follow".to_string())
            .spawn(move || {
                let mut failing = false;
                loop {
                    std::thread::sleep(interval);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    if !refresh_all(&readers, &path, &mut failing) {
                        break;
                    }
                }
                done.store(true, Ordering::Release);
            })?;
        Ok(follow)
    }

    /// Whether the archive is still being followed: the file was there and
    /// some recording was not finalized at the last reopen.
    pub fn active(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }

    /// Reopen every reader now, as the thread does each interval. Returns
    /// whether to keep following; when not, the follow ends.
    #[cfg(test)]
    pub fn refresh_now(&self) -> bool {
        let mut failing = false;
        let more = refresh_all(&self.readers, &self.path, &mut failing);
        if !more {
            self.done.store(true, Ordering::Release);
        }
        more
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.done.store(true, Ordering::Release);
    }
}

/// Reopen each reader. A reader whose reopen fails keeps its previous view,
/// and the failure is logged once until a reopen succeeds again. Returns
/// whether to keep following: false once every recording is finalized, or
/// once the file is gone (hindsight removes its buffer when it exits, without
/// finalizing it), in which case the readers are left as last opened.
fn refresh_all(readers: &[Arc<LiveReader>], path: &std::path::Path, failing: &mut bool) -> bool {
    if !path.exists() {
        info!("{} was removed; no longer following it", path.display());
        return false;
    }
    let mut failed = None;
    for reader in readers {
        if let Err(e) = reader.refresh() {
            failed = Some(e);
        }
    }
    match failed {
        Some(e) if !*failing => {
            warn!("{}: could not reread the archive: {e}", path.display());
            *failing = true;
        }
        Some(_) => {}
        None => *failing = false,
    }
    let more = readers.iter().any(|r| !r.complete());
    if !more {
        info!("{} was finalized; no longer following it", path.display());
    }
    more
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

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
