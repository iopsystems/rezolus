use metriken::AtomicHistogram;
use metriken::LazyCounter;
use std::time::Duration;

/// A `Counter` is a wrapper type that enables us to automatically calculate
/// percentiles for secondly rates between subsequent counter observations.
///
/// To do this, it contains the current reading, previous reading, and
/// optionally a histogram to store rate observations.
pub struct Counter {
    counter: &'static LazyCounter,
    histogram: Option<&'static AtomicHistogram>,
}

impl Counter {
    /// Construct a new counter that wraps a `metriken` counter and optionally a
    /// `metriken` histogram.
    pub fn new(counter: &'static LazyCounter, histogram: Option<&'static AtomicHistogram>) -> Self {
        Self { counter, histogram }
    }

    /// Updates the counter by setting it to a new value. If this counter has a
    /// histogram it also calculates a secondly rate since the last reading
    /// and increments the histogram.
    pub fn set(&mut self, elapsed: f64, value: u64) -> u64 {
        if let Some(histogram) = self.histogram {
            if let Some(previous) =
                metriken::Lazy::<metriken::Counter>::get(self.counter).map(|c| c.value())
            {
                let delta = value.wrapping_sub(previous);

                let _ = histogram.increment((delta as f64 / elapsed) as _);
            }
        }

        self.counter.set(value)
    }

    /// Updates the counter by setting it to a new value. If this counter has a
    /// histogram it also calculates a secondly rate since the last reading
    /// and increments the histogram.
    pub fn set2(&mut self, elapsed: Option<Duration>, value: u64) -> u64 {
        if elapsed.is_some() {
            self.set(elapsed.unwrap().as_secs_f64(), value)
        } else {
            self.counter.set(value)
        }
    }

    /// Updates the counter by incrementing it by some value. If this counter
    /// has a histogram, it normalizes the increment to a secondly rate and
    /// increments the histogram too.
    #[allow(dead_code)]
    pub fn add(&mut self, elapsed: f64, delta: u64) -> u64 {
        if let Some(histogram) = self.histogram {
            let _ = histogram.increment((delta as f64 / elapsed) as _);
        }

        self.counter.add(delta)
    }
}
