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
//! Each subscription runs its own task with its own timer, aligned to ITS
//! interval, and asks for a snapshot when the timer fires. What it gets back
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
use std::time::{Duration, SystemTime};

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
    interval: Duration,
}

impl Subscription {
    pub fn interval(&self) -> Duration {
        self.interval
    }
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
            interval,
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

/// How long until the next wall-clock instant that is a whole multiple of
/// `interval` since the Unix epoch.
///
/// Aligning to absolute time rather than to whenever a subscription opened is
/// what makes two hosts with the same interval sample at the same instant, and
/// so makes their readings correlatable. Residual skew is not hidden by it:
/// each acquisition group carries its own window, so a query can still see
/// exactly when a value was read.
///
/// Note this is the opposite choice from `seal_policy::recording_stagger_key`,
/// which de-synchronizes segment sealing on purpose, and the two do not
/// conflict — sampling is cheap local reads with nothing contended between
/// hosts, while sealing is disk I/O that benefits from spreading.
pub fn until_next_aligned(interval: Duration) -> Duration {
    let now = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos(),
        Err(_) => return interval,
    };
    align_wait(now, interval)
}

/// The pure half of [`until_next_aligned`], split out so alignment can be
/// asserted against a known `now` rather than against a second clock read —
/// which is not the same instant, and so cannot land on a boundary.
fn align_wait(now_ns: u128, interval: Duration) -> Duration {
    let period = interval.as_nanos();
    if period == 0 {
        return interval;
    }
    // Landing exactly on a boundary waits a whole period rather than firing
    // twice at one instant.
    let wait = period - (now_ns % period);
    Duration::from_nanos(wait.min(u64::MAX as u128) as u64)
}

/// Which of a subscription's intervals a given instant belongs to — the value
/// a stream frame carries as `seq`.
pub fn interval_index(wall_ns: u64, interval: Duration) -> u64 {
    let period = (interval.as_nanos() as u64).max(1);
    wall_ns / period
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

    #[test]
    fn alignment_lands_on_wall_clock_multiples() {
        let offsets = [0u128, 1, 999, 1_000_000, 123_456_789, 999_999_999];
        for millis in [1u64, 10, 50, 100, 1_000, 5_000, 97_000] {
            let interval = Duration::from_millis(millis);
            let period = interval.as_nanos();
            for base in offsets {
                let now = 1_700_000_000_000_000_000u128 + base;
                let wait = align_wait(now, interval).as_nanos();
                assert!(
                    wait > 0 && wait <= period,
                    "interval {millis}ms at +{base}ns produced a {wait}ns wait"
                );
                assert_eq!(
                    (now + wait) % period,
                    0,
                    "interval {millis}ms at +{base}ns did not land on a boundary"
                );
            }
        }
    }

    /// The case that killed the shared-clock design: an interval that does not
    /// divide anything else still fires on its OWN boundaries, exactly, with
    /// no jitter — and therefore at the same instants on every host running
    /// it.
    #[test]
    fn a_prime_interval_still_fires_exactly_on_its_own_boundaries() {
        let interval = Duration::from_secs(97);
        let period = interval.as_nanos();

        // Two hosts whose subscriptions opened at unrelated moments.
        for start in [1_700_000_000_123_456_789u128, 1_700_000_055_987_654_321] {
            let mut at = start + align_wait(start, interval).as_nanos();
            let mut fired = Vec::new();
            for _ in 0..5 {
                assert_eq!(at % period, 0, "fired off a 97s boundary");
                fired.push(at);
                at += align_wait(at, interval).as_nanos();
            }
            // Exactly 97s apart, every time — not 95/100 as a shared 5s clock
            // would have been forced into.
            for pair in fired.windows(2) {
                assert_eq!(pair[1] - pair[0], period);
            }
        }
    }

    /// A zero interval cannot be turned into a boundary, and must not divide
    /// by zero. Unreachable in practice; this pins the guard.
    #[test]
    fn a_zero_interval_degrades_instead_of_dividing_by_zero() {
        assert_eq!(align_wait(12_345, Duration::ZERO), Duration::ZERO);
        assert_eq!(interval_index(12_345, Duration::ZERO), 12_345);
    }
}
