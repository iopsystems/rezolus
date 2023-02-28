use crate::Instant;

#[cfg(feature = "bpf")]
pub mod bpf;

#[cfg(not(feature = "bpf"))]
pub mod classic;

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
        	metriken::Heatmap::new(0, 4, 64, Duration::from_secs(60), Duration::from_secs(1)).unwrap()
        })
    };
    ($ident:ident, $name:tt, $description:tt) => {
        #[metriken::metric(
            name = $name,
            description = $description,
            crate = metriken
        )]
        pub static $ident: Lazy<metriken::Heatmap> = metriken::Lazy::new(|| {
        	metriken::Heatmap::new(0, 4, 64, Duration::from_secs(60), Duration::from_secs(1)).unwrap()
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

pub struct Counter {
    previous: Option<u64>,
    counter: &'static metriken::Lazy<metriken::Counter>,
    heatmap: Option<&'static metriken::Lazy<metriken::Heatmap>>,
}

impl Counter {
    pub fn new(counter: &'static metriken::Lazy<metriken::Counter>, heatmap: Option<&'static metriken::Lazy<metriken::Heatmap>>) -> Self {
        Self {
            previous: None,
            counter,
            heatmap,
        }
    }

    pub fn update(&mut self, now: Instant, elapsed_s: f64, current: u64) {
        if let Some(previous) = self.previous {
            let delta = current.wrapping_sub(previous);
            self.counter.add(delta);
            if let Some(heatmap) = self.heatmap {
                heatmap.increment(now, (delta as f64 / elapsed_s) as _, 1);
            }
        }
        self.previous = Some(current);
    }
}

#[cfg(feature = "bpf")]
pub struct Distribution {
    previous: [u64; 496],
    heatmap: &'static metriken::Lazy<metriken::Heatmap>,
    map: &'static str,
}

#[cfg(feature = "bpf")]
impl Distribution {
    pub const fn new(map: &'static str, heatmap: &'static metriken::Lazy<metriken::Heatmap>) -> Self {
        Self {
            previous: [0; 496],
            heatmap,
            map,
        }
    }

    pub fn update(&mut self, obj: &libbpf_rs::Object) {
        let map = obj.map(self.map).unwrap();
        crate::common::bpf::update_histogram_from_dist(map, self.heatmap, &mut self.previous);
    }
}