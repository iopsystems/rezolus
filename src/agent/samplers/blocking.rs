//! Consumer-triggered, nonoverlapping workers for expensive sampler reads.
//!
//! Admission only tries a short scheduling lock; device/state locks belong on
//! the blocking pool. A permit owns the in-flight slot from admission through
//! completion, including queued-task cancellation and unwinding. Metric values,
//! acquisition windows and ordinary read errors remain the sampler's concern.
//!
//! Panic recovery applies only when the process unwinds. The binary's global
//! panic hook exits the process, and this helper does not override that policy.

use crate::{error, warn};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

#[derive(Clone, Copy)]
pub(super) enum Cadence {
    FromDispatch,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    FromCompletion,
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
    warn_after: Option<Duration>,
    schedule: Arc<Mutex<Schedule>>,
}

impl BlockingWorker {
    pub(super) fn new(name: &'static str, interval: Duration, cadence: Cadence) -> Self {
        Self {
            name,
            interval,
            cadence,
            warn_after: None,
            schedule: Arc::new(Mutex::new(Schedule::default())),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn with_stuck_warning(mut self, after: Duration) -> Self {
        self.warn_after = Some(after);
        self
    }

    /// Reserve the sole worker if due, without waiting or advancing the cadence
    /// for a rejected refresh. Clone sampler state only after admission.
    pub(super) fn try_acquire(&self) -> Option<Permit> {
        self.try_acquire_at(Instant::now())
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn is_running(&self) -> bool {
        recover_lock(&self.schedule).running_since.is_some()
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
                    "{}: a blocking sweep has run for {running:?}; no second worker was dispatched",
                    self.name
                );
            }
            return None;
        }
        if schedule
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.interval)
        {
            return None;
        }
        schedule.last = Some(now);
        schedule.running_since = Some(now);
        schedule.stuck_warned = false;
        Some(Permit {
            worker: self.clone(),
            panicked: false,
        })
    }
}

/// Keep the permit in the queued closure, so even cancellation before execution
/// releases it. Dropping a permit without spawning also releases admission.
pub(super) struct Permit {
    worker: BlockingWorker,
    panicked: bool,
}

impl Permit {
    pub(super) fn spawn(mut self, work: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            if catch_unwind(AssertUnwindSafe(work)).is_err() {
                self.panicked = true;
                error!(
                    "{}: sweep panicked; retrying after the configured interval",
                    self.worker.name
                );
            }
            // Keep the permit until all work and error reporting have finished.
            drop(self);
        })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut schedule = recover_lock(&self.worker.schedule);
        // A panic always gets a full retry interval, even if this sampler
        // normally measures cadence from dispatch and the sweep ran longer.
        if self.panicked || matches!(self.worker.cadence, Cadence::FromCompletion) {
            schedule.last = Some(Instant::now());
        }
        schedule.running_since = None;
    }
}

/// Recover state on the blocking worker, retaining slot ownership after an
/// unwind. Callers must tolerate partially updated state; this is not rollback.
pub(super) fn recover_lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|e| {
        state.clear_poison();
        e.into_inner()
    })
}

#[cfg(test)]
mod tests;
