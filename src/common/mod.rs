#[cfg(feature = "bpf")]
pub mod bpf;

pub mod classic;

mod nop;
pub use nop::Nop;

type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;
pub type LazyCounter = metriken::Lazy<metriken::Counter>;
pub type LazyGauge = metriken::Lazy<metriken::Gauge>;
pub type LazyHeatmap = metriken::Lazy<metriken::Heatmap>;

#[allow(dead_code)]
static PAGE_SIZE: once_cell::sync::Lazy<usize> = once_cell::sync::Lazy::new(|| {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
});

#[cfg(target_os = "linux")]
static CACHE_LINESIZE: once_cell::sync::Lazy<usize> = once_cell::sync::Lazy::new(|| {
    unsafe { libc::sysconf(libc::_SC_LEVEL1_DCACHE_LINESIZE) as usize }
});

pub struct Counter {
    previous: Option<u64>,
    counter: &'static LazyCounter,
    heatmap: Option<&'static LazyHeatmap>,
}

impl Counter {
    pub fn new(counter: &'static LazyCounter, heatmap: Option<&'static LazyHeatmap>) -> Self {
        Self {
            previous: None,
            counter,
            heatmap,
        }
    }
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
