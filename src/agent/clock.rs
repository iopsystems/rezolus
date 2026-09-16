//! The agent's sampling clock — which exists only while something needs it.
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
//! So demand creates the clock and the last subscription leaving destroys it.
//!
//! # One pass feeds everyone
//!
//! Subscribers register an interval; the clock runs at the fastest of them
//! (clamped by a configured floor) and samples ONCE per tick. A subscriber
//! wanting a slower rate takes every Nth snapshot rather than causing its own
//! sampling pass — which is the property that lets hindsight, a recorder and a
//! live viewer share one agent without multiplying its cost.
//!
//! # Aligned to the wall clock
//!
//! Ticks land on wall-clock multiples of the interval since the Unix epoch, so
//! a 1 s clock fires at the top of each second and every host in a fleet
//! samples at the same instant — which is what makes readings correlatable
//! across hosts. Residual skew is not hidden by this: each acquisition group
//! still carries its own window, so a query can see exactly when a value was
//! read.
//!
//! Note this is the opposite choice from `seal_policy::recording_stagger_key`,
//! which deliberately de-synchronizes segment sealing, and the two do not
//! conflict: sampling is cheap local reads with nothing contended between
//! hosts, while sealing is disk I/O that benefits from spreading.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime};

use tokio::sync::{watch, Mutex, Notify};

use super::exposition::http::SnapshotBuilder;

/// Registered demand for periodic sampling.
struct Inner {
    /// Live subscriptions: id -> the interval each asked for. A `BTreeMap`
    /// rather than a counter because the clock's rate is the MINIMUM of what
    /// is asked for, so a subscription leaving can slow the clock down, which
    /// needs the individual values.
    demand: StdMutex<BTreeMap<u64, Duration>>,
    next_id: AtomicU64,
    /// Woken when `demand` changes, so the driver recomputes its rate rather
    /// than waiting out a tick at the old one.
    changed: Notify,
    /// The fastest interval any subscriber can obtain. A subscriber is remote
    /// and untrusted with respect to this agent's CPU budget; without a floor
    /// it could ask for 1 µs and spin the sampling loop.
    floor: Duration,
    /// Incremented after every completed sampling pass. Subscribers watch this
    /// rather than polling the builder, so a tick wakes them exactly once.
    generation: watch::Sender<u64>,
}

/// A handle on the agent's sampling clock.
#[derive(Clone)]
pub struct SampleClock {
    inner: Arc<Inner>,
}

/// One subscriber's demand. Sampling continues while at least one of these is
/// alive; dropping the last one returns the agent to tickless.
pub struct Subscription {
    inner: Arc<Inner>,
    id: u64,
    interval: Duration,
}

impl Subscription {
    /// The interval this subscription asked for, after the floor was applied.
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// A receiver that ticks once per completed sampling pass.
    pub fn generation(&self) -> watch::Receiver<u64> {
        self.inner.generation.subscribe()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.inner
            .demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        // Wake the driver even when demand is now empty — that is exactly the
        // transition it must notice in order to stop sampling.
        //
        // `notify_one`, NOT `notify_waiters`: the latter wakes only waiters
        // already registered, so a change landing before the driver parks is
        // simply lost. `notify_one` stores a permit, and the driver re-reads
        // demand from scratch on every wakeup, so one permit is enough to
        // convey any number of changes.
        self.inner.changed.notify_one();
    }
}

impl SampleClock {
    pub fn new(floor: Duration) -> Self {
        let (generation, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                demand: StdMutex::new(BTreeMap::new()),
                next_id: AtomicU64::new(0),
                changed: Notify::new(),
                floor,
                generation,
            }),
        }
    }

    /// Ask for sampling at `interval`, clamped to the configured floor.
    ///
    /// The clock starts if it was idle. It stops when the returned
    /// [`Subscription`] and every other one is dropped.
    pub fn subscribe(&self, interval: Duration) -> Subscription {
        let interval = interval.max(self.inner.floor);
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, interval);
        // See the note in `Subscription::drop`: a permit must survive a driver
        // that has not parked yet, or the very first subscription can be lost
        // and the agent never starts sampling.
        self.inner.changed.notify_one();
        Subscription {
            inner: Arc::clone(&self.inner),
            id,
            interval,
        }
    }

    /// How many subscriptions are currently driving the clock.
    pub fn subscribers(&self) -> usize {
        self.inner
            .demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// The rate the clock is running at, or `None` when nothing is asking —
    /// which is to say, when the agent is tickless.
    pub fn current(&self) -> Option<Duration> {
        self.inner
            .demand
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .min()
            .copied()
    }

    /// Drive the clock until the process ends.
    ///
    /// With no demand this parks on a `Notify` and costs nothing — no timer is
    /// armed, so there is no wakeup to charge to an idle core.
    pub async fn run(self, builder: Arc<Mutex<SnapshotBuilder>>) {
        loop {
            // Register interest BEFORE reading demand. A subscription arriving
            // between the read and the await would otherwise notify nobody and
            // leave the clock parked until the next unrelated change.
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);

            let Some(interval) = self.current() else {
                changed.await;
                continue;
            };

            tokio::select! {
                _ = tokio::time::sleep(until_next_aligned(interval)) => {
                    builder.lock().await.sample().await;
                    // `send_modify` rather than `send`: it publishes even with
                    // no receivers attached, so a tick driven purely by a
                    // hindsight subscription is still counted.
                    self.inner.generation.send_modify(|g| *g += 1);
                }
                _ = &mut changed => {}
            }
        }
    }
}

/// How long until the next wall-clock instant that is a whole multiple of
/// `interval` since the Unix epoch.
///
/// Returns `interval` itself if the clock is unreadable, which only degrades
/// alignment rather than stopping the tick.
fn until_next_aligned(interval: Duration) -> Duration {
    let now = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos(),
        Err(_) => return interval,
    };
    align_wait(now, interval)
}

/// The pure half of [`until_next_aligned`], split out so the alignment can be
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a clock and a snapshot builder wired together, and return a
    /// handle on each.
    fn rig(floor: Duration) -> (SampleClock, Arc<Mutex<SnapshotBuilder>>) {
        use crate::agent::config::Config;
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\n").expect("valid config");
        let builder = Arc::new(Mutex::new(SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn crate::agent::Sampler>>::new().into_boxed_slice()),
            None,
        )));
        (SampleClock::new(floor), builder)
    }

    /// Advance simulated time in `steps` of `step`, yielding between each so
    /// the driver task can arm its next timer.
    ///
    /// A single large `advance` would not do: the driver arms its timer when
    /// it is polled, so jumping an hour before it has ever run leaves the
    /// timer armed AFTER the jump and nothing fires.
    async fn advance_stepwise(step: Duration, steps: u32) {
        for _ in 0..steps {
            tokio::task::yield_now().await;
            tokio::time::advance(step).await;
        }
        tokio::task::yield_now().await;
    }

    /// **The property this module exists for.** An agent nobody is subscribed
    /// to performs zero sampling passes, however long it runs.
    ///
    /// Not a micro-optimization: #1226 measured a cross-CPU perf read at 1002
    /// µs p50 idle against 52 µs under load, because the read charges the
    /// target core's C-state exit latency. An agent that sampled on a timer
    /// regardless of demand would wake every idle core to observe that it was
    /// idle — an observer effect, and worst exactly where nobody asked.
    #[tokio::test(start_paused = true)]
    async fn an_unsubscribed_agent_never_samples() {
        let (clock, builder) = rig(Duration::from_millis(10));
        tokio::spawn(clock.clone().run(builder.clone()));

        assert_eq!(clock.current(), None);
        // An hour of simulated time, stepwise, so a timer armed at ANY point
        // would have had ample opportunity to fire.
        advance_stepwise(Duration::from_secs(1), 3600).await;

        assert_eq!(
            builder.lock().await.samples(),
            0,
            "an agent with no subscriber must not sample"
        );
    }

    /// ...and a subscription is what starts it, so the tickless property is
    /// not simply a broken clock.
    #[tokio::test(start_paused = true)]
    async fn a_subscription_starts_the_clock_and_dropping_it_stops_again() {
        let (clock, builder) = rig(Duration::from_millis(10));
        tokio::spawn(clock.clone().run(builder.clone()));

        let subscription = clock.subscribe(Duration::from_millis(100));
        advance_stepwise(Duration::from_millis(100), 20).await;

        let while_subscribed = builder.lock().await.samples();
        assert!(
            while_subscribed > 0,
            "a subscriber must actually get sampling; saw {while_subscribed}"
        );

        drop(subscription);
        // Let the driver notice the change and park before we measure.
        advance_stepwise(Duration::from_millis(1), 2).await;
        let at_unsubscribe = builder.lock().await.samples();

        advance_stepwise(Duration::from_secs(1), 60).await;

        assert_eq!(
            builder.lock().await.samples(),
            at_unsubscribe,
            "the last subscription leaving must return the agent to tickless"
        );
    }

    /// A subscriber arriving at an agent that has been idle for a while must
    /// start the clock. This is the ordinary case — an agent runs untouched
    /// until someone records it — and it is the one where the driver is
    /// genuinely parked rather than merely not yet polled.
    ///
    /// Note what this does NOT cover: a change landing in the window between
    /// the driver reading demand and registering on the `Notify`. That window
    /// is why `notify_one` is used rather than `notify_waiters` — the former
    /// stores a permit, the latter wakes only already-registered waiters — and
    /// it is not deterministically reachable from a test, so the reasoning is
    /// recorded at the call sites rather than asserted here.
    #[tokio::test(start_paused = true)]
    async fn a_subscriber_arriving_at_an_idle_agent_starts_the_clock() {
        let (clock, builder) = rig(Duration::from_millis(10));
        tokio::spawn(clock.clone().run(builder.clone()));

        // Let the driver run and park with nothing to do.
        advance_stepwise(Duration::from_secs(1), 30).await;
        assert_eq!(builder.lock().await.samples(), 0);

        let _subscription = clock.subscribe(Duration::from_millis(100));
        advance_stepwise(Duration::from_millis(100), 20).await;

        assert!(
            builder.lock().await.samples() > 0,
            "a subscriber arriving at a parked clock must start it"
        );
    }

    #[test]
    fn the_floor_clamps_a_subscriber_that_asks_for_too_much() {
        let clock = SampleClock::new(Duration::from_millis(10));
        let fast = clock.subscribe(Duration::from_micros(1));
        assert_eq!(fast.interval(), Duration::from_millis(10));
        assert_eq!(clock.current(), Some(Duration::from_millis(10)));
    }

    #[test]
    fn the_clock_runs_at_the_fastest_subscriber_and_slows_when_it_leaves() {
        let clock = SampleClock::new(Duration::from_millis(10));
        assert_eq!(clock.current(), None, "no demand means no clock");

        let slow = clock.subscribe(Duration::from_secs(1));
        assert_eq!(clock.current(), Some(Duration::from_secs(1)));

        let fast = clock.subscribe(Duration::from_millis(50));
        assert_eq!(
            clock.current(),
            Some(Duration::from_millis(50)),
            "one pass has to satisfy the fastest subscriber"
        );

        // The slow subscriber must not hold the clock at 50ms once the fast
        // one leaves — which is why demand is a map of intervals rather than a
        // count.
        drop(fast);
        assert_eq!(clock.current(), Some(Duration::from_secs(1)));

        drop(slow);
        assert_eq!(
            clock.current(),
            None,
            "the last subscription leaving is tickless again"
        );
    }

    #[test]
    fn alignment_lands_on_wall_clock_multiples() {
        // Whatever the time, the wait must carry us to a boundary — that is
        // what makes two hosts sample at the same instant.
        let offsets = [0u128, 1, 999, 1_000_000, 123_456_789, 999_999_999];
        for millis in [1u64, 10, 50, 100, 1_000, 5_000] {
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

    /// A zero interval cannot be turned into a boundary, and must not divide
    /// by zero. Unreachable through `subscribe` (the floor is non-zero), so
    /// this pins the guard rather than a behaviour anyone relies on.
    #[test]
    fn a_zero_interval_degrades_instead_of_dividing_by_zero() {
        assert_eq!(align_wait(12_345, Duration::ZERO), Duration::ZERO);
    }
}
