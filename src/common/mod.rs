#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf;

pub mod classic;
pub mod units;

mod interval;
mod nop;

use metriken::AtomicHistogram;
use metriken::LazyCounter;

pub use interval::Interval;
pub use nop::Nop;

pub const HISTOGRAM_GROUPING_POWER: u8 = 7;

/// A `Counter` is a wrapper type that enables us to automatically calculate
/// percentiles for secondly rates between subsequent counter observations.
///
/// To do this, it contains the current reading, previous reading, and
/// optionally a histogram to store rate observations.
pub struct Counter {
    previous: Option<u64>,
    counter: &'static LazyCounter,
    histogram: Option<&'static AtomicHistogram>,
}

impl Counter {
    /// Construct a new counter that wraps a `metriken` counter and optionally a
    /// `metriken` histogram.
    pub fn new(counter: &'static LazyCounter, histogram: Option<&'static AtomicHistogram>) -> Self {
        Self {
            previous: None,
            counter,
            histogram,
        }
    }

    /// Updates the counter by setting the current value to a new value. If this
    /// counter has a histogram it also calculates the rate since the last reading
    /// and increments the histogram.
    pub fn set(&mut self, elapsed: f64, value: u64) {
        if let Some(previous) = self.previous {
            let delta = value.wrapping_sub(previous);
            self.counter.add(delta);
            if let Some(histogram) = self.histogram {
                let _ = histogram.increment((delta as f64 / elapsed) as _);
            }
        }
        self.previous = Some(value);
    }
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for defining a top-level sampler which will contain
/// other samplers. For instance, this is used for the top-level `cpu` sampler
/// which then contains other related samplers for perf events, cpu usage, etc.
macro_rules! sampler {
    ($ident:ident, $name:tt, $slice:ident) => {
        #[distributed_slice]
        pub static $slice: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

        #[distributed_slice(SAMPLERS)]
        fn init(config: &Config) -> Box<dyn Sampler> {
            Box::new($ident::new(config))
        }

        pub struct $ident {
            samplers: Vec<Box<dyn Sampler>>,
        }

        impl $ident {
            fn new(config: &Config) -> Self {
                let samplers = $slice.iter().map(|init| init(config)).collect();
                Self {
                    samplers,
                }
            }
        }

        impl Sampler for $ident {
            fn sample(&mut self) {
                for sampler in &mut self.samplers {
                    sampler.sample()
                }
            }
        }
    };
}
