mod counters;
mod gauges;

#[allow(unused_imports)]
pub use counters::{CounterGroup, CounterGroupError};

#[allow(unused_imports)]
pub use gauges::{GaugeGroup, GaugeGroupError};

use std::collections::HashMap;

#[allow(dead_code)]
pub trait MetricGroup: Sync {
    fn insert_metadata(&self, idx: usize, key: String, value: String);
    fn load_metadata(&self, idx: usize) -> Option<HashMap<String, String>>;
    fn clear_metadata(&self, idx: usize);
    fn len(&self) -> usize;
}
