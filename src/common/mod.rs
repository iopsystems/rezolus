// Not all items are used on all platforms / configurations. Ignore any warnings
// for dead code.
#![allow(dead_code)]

#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf;

#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf2;

pub mod classic;
pub mod units;

mod counter;
mod interval;
mod nop;

pub use clocksource::precise::UnixInstant;
use core::sync::atomic::AtomicBool;
pub use counter::Counter;
pub use interval::{AsyncInterval, Interval};
pub use nop::Nop;
use parking_lot::Condvar;
use parking_lot::Mutex;
use std::sync::Arc;

pub const HISTOGRAM_GROUPING_POWER: u8 = 7;

use tokio::sync::Notify;

/// A struct which allows for triggering and asynchronously waiting for
/// notification from a remote thread. This uses `parking_lot::Mutex` and
/// `parking_lot::CondVar` to block and trigger the synchronous thread. The
/// thread can then provide a notification to the task when it has completed.
#[derive(Clone)]
pub struct SyncPrimitive {
    initialized: Arc<AtomicBool>,
    trigger: Arc<(Mutex<bool>, Condvar)>,
    notify: Arc<Notify>,
}

impl SyncPrimitive {
    pub fn new() -> Self {
        let initialized = Arc::new(AtomicBool::new(false));
        let trigger = Arc::new((Mutex::new(false), Condvar::new()));
        let notify = Arc::new(Notify::new());

        Self {
            initialized,
            trigger,
            notify,
        }
    }

    /// Trigger the remote thread waiting on the condition variable.
    pub fn trigger(&self) {
        let (lock, cvar) = &*self.trigger;
        let mut started = lock.lock();
        *started = true;
        cvar.notify_one();
    }

    /// Block the thread until triggered. Uses `parking_lot::CondVar` to block
    /// the thread so it consumes no CPU time while waiting.
    pub fn wait_trigger(&self) {
        let (lock, cvar) = &*self.trigger;
        let mut started = lock.lock();
        if !*started {
            cvar.wait(&mut started);
        }
    }

    /// Notify the async task that the thread has completed its work.
    pub fn notify(&self) {
        let (lock, _cvar) = &*self.trigger;
        let mut running = lock.lock();
        *running = false;
        self.notify.notify_one();
    }

    /// Wait to be notified that the remote thread has completed its work.
    pub async fn wait_notify(&self) {
        self.notify.notified().await;
    }
}
