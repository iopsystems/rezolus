#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf;

pub mod classic;

mod nop;

use metriken::AtomicHistogram;
use metriken::LazyCounter;
pub use nop::Nop;

/// A `Counter` is a wrapper type that enables us to automatically calculate
/// percentiles for secondly rates between subsequent counter observations.
///
/// To do this, it contains the current reading, previous reading, and
/// optionally a heatmap to store rate observations.
pub struct Counter {
    previous: Option<u64>,
    counter: &'static LazyCounter,
    heatmap: Option<&'static AtomicHistogram>,
}

impl Counter {
    /// Construct a new counter that wraps a `metriken` counter and optionally a
    /// `metriken` heatmap.
    pub fn new(counter: &'static LazyCounter, heatmap: Option<&'static AtomicHistogram>) -> Self {
        Self {
            previous: None,
            counter,
            heatmap,
        }
    }

    /// Updates the counter by setting the current value to a new value. If this
    /// counter has a heatmap it also calculates the rate since the last reading
    /// and increments the heatmap.
    pub fn set(&mut self, elapsed: f64, value: u64) {
        if let Some(previous) = self.previous {
            let delta = value.wrapping_sub(previous);
            self.counter.add(delta);
            if let Some(heatmap) = self.heatmap {
                let _ = heatmap.increment((delta as f64 / elapsed) as _);
            }
        }
        self.previous = Some(value);
    }
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized
/// `metriken::Counter` given an identifier, name, and optional description.
macro_rules! counter {
    ($ident:ident, $name:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Counter> = metriken::Lazy::new(|| {
        	metriken::Counter::new()
        });
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Counter> = metriken::Lazy::new(|| {
        	metriken::Counter::new()
        });
    };
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized
/// `metriken::Gauge` given an identifier, name, and optional description.
macro_rules! gauge {
    ($ident:ident, $name:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Gauge> = metriken::Lazy::new(|| {
            metriken::Gauge::new()
        });
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Gauge> = metriken::Lazy::new(|| {
            metriken::Gauge::new()
        });
    };
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized
/// `metriken::Heatmap` given an identifier, name, and optional description.
///
/// The heatmap configuration used here can record counts for all 64bit integer
/// values with a maximum error of 0.78%. The heatmap covers a moving window of
/// one minute with one second resolution.
macro_rules! heatmap {
    ($ident:ident, $name:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: metriken::AtomicHistogram = metriken::AtomicHistogram::new(7, 64);
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            description = $description,
            crate = metriken
        )]
        pub static $ident: metriken::AtomicHistogram = metriken::AtomicHistogram::new(7, 64);
    };
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized
/// `metriken::Heatmap` given an identifier, name, and optional description.
///
/// The heatmap configuration used here can record counts for all 64bit integer
/// values with a maximum error of 0.78%. The heatmap covers a moving window of
/// one minute with one second resolution.
macro_rules! bpfhistogram {
    ($ident:ident, $name:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: metriken::RwLockHistogram = metriken::RwLockHistogram::new(7, 64);
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            description = $description,
            crate = metriken
        )]
        pub static $ident: metriken::RwLockHistogram = metriken::RwLockHistogram::new(7, 64);
    };
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized counter with a
/// heatmap which will track secondly rates for the same counter.
macro_rules! counter_with_heatmap {
	($counter:ident, $heatmap:ident, $name:tt) => {
		self::counter!($counter, $name);
		self::heatmap!($heatmap, $name);
	};
	($counter:ident, $heatmap:ident, $name:tt, $description:tt) => {
		self::counter!($counter, $name, $description);
		self::heatmap!($heatmap, $name, $description);
	}
}

#[macro_export]
#[rustfmt::skip]
/// A convenience macro for constructing a lazily initialized gauge with a
/// heatmap which will track instantaneous readings for the same gauge.
macro_rules! gauge_with_heatmap {
    ($gauge:ident, $heatmap:ident, $name:tt) => {
        self::gauge!($gauge, $name);
        self::heatmap!($heatmap, $name);
    };
    ($gauge:ident, $heatmap:ident, $name:tt, $description:tt) => {
        self::gauge!($gauge, $name, $description);
        self::heatmap!($heatmap, $name, $description);
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
