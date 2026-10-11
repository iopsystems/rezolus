//! Subscription cadence: each subscriber keeps its own aligned timer, and the
//! snapshot TTL decides whether a tick costs a sampling pass.
//!
//! # Tickless by default, and not as an optimization
//!
//! Nothing here runs unless something has asked for periodic sampling. An
//! agent serving only `/metrics/binary` keeps the behaviour it has always had:
//! a scrape arrives, the TTL has expired, the samplers run. **The request is
//! the clock.**
//!
//! That is deliberate and it is about measurement fidelity, not CPU. #1226
//! measured a cross-CPU perf read at **1002 µs p50 idle against 52 µs under
//! load — 19.3x** — because a perf event opened on another CPU and read from
//! this one goes through `smp_call_function_single`, and a target core in a
//! deep idle state charges its C-state exit latency to the read. An agent that
//! ticked unconditionally would wake every idle core once a second in order to
//! observe that they were idle: the issue's own words, *"an observer effect,
//! not just overhead"*. The expensive case is precisely the one where nobody
//! asked.
//!
//! # No shared clock
//!
//! Each subscription runs its own timer, aligned to ITS interval since the
//! Unix epoch (metriken-exposition's `StreamSubscription::into_stream`), and
//! asks for a snapshot when the timer fires. What it gets back
//! is either a fresh sampling pass or the cached one, exactly as a scrape
//! would — [`SnapshotBuilder::build`] has always made that decision, and it is
//! the natural coalescing point.
//!
//! An earlier design gave the agent one clock running at the fastest demand
//! and had slower subscribers drop the ticks in between. That needed
//! decimation, a rule for what to do when the intervals did not divide (97 s
//! against a 5 s clock), and a floor to stop a subscriber spinning the loop.
//! None of it is needed here:
//!
//! - **Alignment is exact for every subscriber**, not quantized to a shared
//!   tick. A 97 s subscription fires on 97 s boundaries since the epoch, so
//!   every host running one fires at the same instants — the property that
//!   makes readings correlatable across a fleet. Under a shared 5 s clock the
//!   same subscription could only be served every 95 s or 100 s, phased to
//!   whenever its connection opened.
//!
//! - **Intervals need not relate to each other at all.** There is no minimum
//!   to compute, nothing to decimate, and a subscription arriving or leaving
//!   changes nothing for anyone else.
//!
//! - **The TTL is the floor.** A subscription asking for less than the TTL
//!   finds the cached snapshot still valid, so it cannot make the agent sample
//!   faster than an operator configured it to. The stream answers such a tick
//!   with an empty frame rather than repeating readings the subscriber
//!   already has, so asking too fast costs neither sampling passes nor
//!   meaningful bandwidth. No second knob.
//!
//! What it costs is that two subscriptions whose timers fall outside one
//! TTL of each other cause two passes where a shared clock would have caused
//! one. That is the right trade: each is answered with a reading taken when it
//! asked, rather than one taken up to a full tick earlier and relabelled.
//!
//! [`SnapshotBuilder::build`]: crate::agent::exposition::http::SnapshotBuilder::build

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Who is currently subscribed, for reporting on `/status`.
///
/// Carries no scheduling role — each subscription drives its own timer. It
/// exists because a subsystem nobody can see is one nobody can debug: "is
/// anything subscribed to this agent, and how fast" is the first question when
/// an agent is sampling more than expected.
#[derive(Clone, Default)]
pub struct Subscribers {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    live: Mutex<BTreeMap<u64, Duration>>,
    next_id: AtomicU64,
}

/// One live subscription. Registered for the life of this value.
pub struct Subscription {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl Subscribers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, interval: Duration) -> Subscription {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, interval);
        Subscription {
            inner: Arc::clone(&self.inner),
            id,
        }
    }

    pub fn count(&self) -> usize {
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The shortest interval anyone asked for, or `None` when nothing is
    /// subscribed — which is to say, when the agent is tickless.
    pub fn fastest(&self) -> Option<Duration> {
        self.inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .min()
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_tracks_who_is_subscribed_and_how_fast() {
        let subs = Subscribers::new();
        assert_eq!(subs.count(), 0);
        assert_eq!(subs.fastest(), None, "nothing subscribed means tickless");

        let slow = subs.register(Duration::from_secs(97));
        assert_eq!(subs.fastest(), Some(Duration::from_secs(97)));

        let fast = subs.register(Duration::from_secs(5));
        assert_eq!(subs.count(), 2);
        assert_eq!(subs.fastest(), Some(Duration::from_secs(5)));

        drop(fast);
        assert_eq!(
            subs.fastest(),
            Some(Duration::from_secs(97)),
            "the remaining subscription's own rate, not the departed one's"
        );
        drop(slow);
        assert_eq!(subs.count(), 0);
        assert_eq!(subs.fastest(), None);
    }
}
