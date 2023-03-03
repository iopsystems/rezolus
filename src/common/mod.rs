#[cfg(feature = "bpf")]
pub mod bpf;

#[cfg(not(feature = "bpf"))]
pub mod classic;

type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;
type LazyCounter = metriken::Lazy<metriken::Counter>;
type LazyHeatmap = metriken::Lazy<metriken::Heatmap>;

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