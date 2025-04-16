use std::collections::hash_map::Entry;
use super::*;

mod counter;
mod gauge;
mod histogram;
mod untyped;

pub use counter::CounterCollection;
pub use gauge::GaugeCollection;
pub use histogram::HistogramCollection;
pub use untyped::UntypedCollection;

