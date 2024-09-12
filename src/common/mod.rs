#[cfg(all(feature = "bpf", target_os = "linux"))]
pub mod bpf;

pub mod classic;
pub mod units;

mod counter;
mod interval;
mod nop;

pub use clocksource::precise::UnixInstant;
pub use counter::Counter;
pub use interval::{AsyncInterval, Interval};
pub use nop::Nop;

// the grouping power must match what we use in the BPF samplers and limits the
// value grouping to a 12.5% relative error. This uses only 4KiB per histogram.
pub const HISTOGRAM_GROUPING_POWER: u8 = 3;
