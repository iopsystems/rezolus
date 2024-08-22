#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf;

pub mod classic;
pub mod units;

mod counter;
mod interval;
mod nop;

pub use clocksource::precise::UnixInstant;
pub use counter::Counter;
pub use interval::Interval;
pub use nop::Nop;

pub const HISTOGRAM_GROUPING_POWER: u8 = 7;

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
