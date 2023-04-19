#[cfg(feature = "bpf")]
pub mod bpf;

pub mod classic;

mod nop;

pub use nop::Nop;

type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;

pub type LazyCounter = metriken::Lazy<metriken::Counter>;
pub type LazyGauge = metriken::Lazy<metriken::Gauge>;
pub type LazyHeatmap = metriken::Lazy<metriken::Heatmap>;

/// A `Counter` is a wrapper type that enables us to automatically calculate
/// percentiles for secondly rates between subsequent counter observations.
///
/// To do this, it contains the current reading, previous reading, and
/// optionally a heatmap to store rate observations.
pub struct Counter {
    previous: Option<u64>,
    counter: &'static LazyCounter,
    heatmap: Option<&'static LazyHeatmap>,
}

impl Counter {
    /// Construct a new counter that wraps a `metriken` counter and optionally a
    /// `metriken` heatmap.
    pub fn new(counter: &'static LazyCounter, heatmap: Option<&'static LazyHeatmap>) -> Self {
        Self {
            previous: None,
            counter,
            heatmap,
        }
    }

    /// Updates the counter by setting the current value to a new value. If this
    /// counter has a heatmap it also calculates the rate since the last reading
    /// and increments the heatmap.
    pub fn set(&mut self, now: Instant, elapsed: f64, value: u64) {
        if let Some(previous) = self.previous {
            let delta = value.wrapping_sub(previous);
            self.counter.add(delta);
            if let Some(heatmap) = self.heatmap {
                heatmap.increment(now, (delta as f64 / elapsed) as _, 1);
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
macro_rules! heatmap {
    ($ident:ident, $name:tt) => {
        #[metriken::metric(
            name = $name,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Heatmap> = metriken::Lazy::new(|| {
        	metriken::Heatmap::new(0, 8, 64, Duration::from_secs(60), Duration::from_secs(1)).unwrap()
        });
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            description = $description,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Heatmap> = metriken::Lazy::new(|| {
        	metriken::Heatmap::new(0, 8, 64, Duration::from_secs(60), Duration::from_secs(1)).unwrap()
        });
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
