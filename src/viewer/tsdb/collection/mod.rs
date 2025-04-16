use std::collections::hash_map::Entry;
use super::*;

mod counter;
mod gauges;
mod histograms;
mod untyped;

pub use counter::CounterCollection;
pub use gauges::GaugeCollection;
pub use histograms::HistogramCollection;
pub use untyped::UntypedCollection;

