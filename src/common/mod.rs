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

pub const HISTOGRAM_GROUPING_POWER: u8 = 7;
