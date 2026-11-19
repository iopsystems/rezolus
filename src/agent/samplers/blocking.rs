//! Consumer-triggered, nonoverlapping workers for expensive sampler reads.
//!
//! Admission only tries a short scheduling lock; device/state locks belong on
//! the blocking pool. A permit admits one read at a time: it is held from
//! admission until the read finishes, is cancelled while queued, or unwinds.
//! Metric values, acquisition windows and ordinary read errors are handled by
//! the sampler. Each sampler has its own worker.
//!
//! Panic recovery applies only when the process unwinds. The binary's global
//! panic hook exits the process, and this helper does not override that policy.

use crate::{error, warn};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// How early a scrape may arrive and still be admitted, for samplers that pass
/// it to [`BlockingWorker::with_tolerance`].
///
/// Scrape timers jitter by well under a millisecond in practice. 50ms is far
/// above that and at most 5% of the default interval of every sampler that
/// uses it (1s for the shortest).
pub(super) const SCRAPE_TOLERANCE: Duration = Duration::from_millis(50);

/// When the next read is due after a read that ran. A worker built with
/// [`BlockingWorker::with_tolerance`] admits each read up to that tolerance,
/// capped at half the interval, before the time described here.
#[derive(Clone, Copy)]
pub(super) enum Cadence {
    /// One interval after the previous read was dispatched. After a read that
    /// ran for longer than the interval, the next one is due when it finishes.
    FromDispatch,
    /// One interval after the previous read was dispatched, or, when that read
    /// finished a full interval or more after its dispatch, one interval after
    /// it finished, so an overrunning read is followed by a full interval with
    /// no read.
    IdleAfterOverrun,
}

#[derive(Default)]
struct Schedule {
    last: Option<Instant>,
    running_since: Option<Instant>,
    stuck_warned: bool,
}

#[derive(Clone)]
pub(super) struct BlockingWorker {
    name: &'static str,
    interval: Duration,
    cadence: Cadence,
    tolerance: Duration,
    warn_after: Option<Duration>,
    schedule: Arc<Mutex<Schedule>>,
}

impl BlockingWorker {
    pub(super) fn new(name: &'static str, interval: Duration, cadence: Cadence) -> Self {
        Self {
            name,
            interval,
            cadence,
            tolerance: Duration::ZERO,
            warn_after: None,
            schedule: Arc::new(Mutex::new(Schedule::default())),
        }
    }

    /// Admit a read up to `tolerance` before the interval has elapsed, so that
    /// a consumer polling at the interval is not rejected by timer jitter. The
    /// tolerance applies up to half the interval, so reads stay at least half
    /// an interval apart from dispatch to dispatch.
    pub(super) fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Warn once when a read has been running for longer than `after`; the
    /// warning resets when the next read is admitted.
    pub(super) fn with_stuck_warning(mut self, after: Duration) -> Self {
        self.warn_after = Some(after);
        self
    }

    /// Reserve the sole worker if due, without waiting or advancing the cadence
    /// for a rejected refresh. Clone sampler state only after admission.
    pub(super) fn try_acquire(&self) -> Option<Permit> {
        self.try_acquire_at(Instant::now())
    }

    #[cfg(test)]
    pub(super) fn is_running(&self) -> bool {
        recover_lock(self.name, &self.schedule)
            .running_since
            .is_some()
    }

    fn try_acquire_at(&self, now: Instant) -> Option<Permit> {
        let mut schedule = match self.schedule.try_lock() {
            Ok(schedule) => schedule,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(e)) => {
                self.schedule.clear_poison();
                e.into_inner()
            }
        };
        if let Some(started) = schedule.running_since {
            let running = now.saturating_duration_since(started);
            if self.warn_after.is_some_and(|after| running > after) && !schedule.stuck_warned {
                schedule.stuck_warned = true;
                warn!(
                    "{}: a sweep has been running for {running:?}; its values hold until it \
                     returns, and no other sweep starts",
                    self.name
                );
            }
            return None;
        }
        let due = self
            .interval
            .saturating_sub(self.tolerance.min(self.interval / 2));
        if schedule
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < due)
        {
            return None;
        }
        let previous_last = schedule.last.replace(now);
        schedule.running_since = Some(now);
        schedule.stuck_warned = false;
        Some(Permit {
            worker: self.clone(),
            previous_last,
            outcome: Some(Outcome::Queued),
        })
    }
}

/// `spawn` moves the permit into the blocking closure, so a task cancelled
/// before it runs drops the permit and restores the previous cadence anchor.
/// Dropping an unspawned permit does the same. Once the closure starts, the
/// sampler's cadence applies whether or not the read returns an error; after a
/// read that unwinds, the next one is due one interval (less any tolerance)
/// after the unwind.
pub(super) struct Permit {
    worker: BlockingWorker,
    previous_last: Option<Instant>,
    outcome: Option<Outcome>,
}

#[derive(Clone, Copy)]
enum Outcome {
    Queued,
    Ran,
    Panicked,
}

impl Permit {
    pub(super) fn spawn(mut self, work: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            self.outcome = Some(Outcome::Ran);
            if catch_unwind(AssertUnwindSafe(work)).is_err() {
                self.outcome = Some(Outcome::Panicked);
                error!(
                    "{}: sweep unwound; next admission delayed by the configured interval, less any \
                     tolerance",
                    self.worker.name
                );
            }
            // Keep the permit until all work and error reporting have finished.
            drop(self);
        })
    }

    fn finish_at(&mut self, now: Instant) {
        let Some(outcome) = self.outcome.take() else {
            return;
        };
        // This short lock is taken on completion, not around device work. An
        // admission racing it may skip one consumer tick instead of waiting.
        let mut schedule = recover_lock(self.worker.name, &self.worker.schedule);
        match outcome {
            Outcome::Queued => schedule.last = self.previous_last,
            // An unwinding panic anchors the retry on the unwind, including for
            // a dispatch-based sampler whose sweep ran longer than its interval.
            Outcome::Panicked => schedule.last = Some(now),
            Outcome::Ran
                if matches!(self.worker.cadence, Cadence::IdleAfterOverrun)
                    && schedule.running_since.is_some_and(|dispatched| {
                        now.saturating_duration_since(dispatched) >= self.worker.interval
                    }) =>
            {
                schedule.last = Some(now);
            }
            Outcome::Ran => {}
        }
        schedule.running_since = None;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.finish_at(Instant::now());
    }
}

/// Lock `state`, clearing poison left by an unwound read and keeping what that
/// read wrote (for example, `hw_sensors`' and `memory_cgroup`'s metric slot
/// assignments). Partial writes are not rolled back.
pub(super) fn recover_lock<'a, T>(name: &str, state: &'a Mutex<T>) -> MutexGuard<'a, T> {
    state.lock().unwrap_or_else(|e| {
        state.clear_poison();
        warn!("{name}: recovering poisoned state after an unwind; retaining existing state");
        e.into_inner()
    })
}

#[cfg(test)]
#[path = "blocking/tests.rs"]
mod tests;
