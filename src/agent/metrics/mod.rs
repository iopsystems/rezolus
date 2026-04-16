#[cfg(target_os = "linux")]
use metriken::{CounterGroup, GaugeGroup};

/// Trait for group metrics that support per-entry metadata management.
///
/// This is used by BPF ringbuffer handlers to attach cgroup/task metadata to
/// group metric entries. Both `CounterGroup` and `GaugeGroup` from metriken
/// have these methods as inherent impls; this trait allows them to be used
/// as trait objects in heterogeneous slices.
#[cfg(target_os = "linux")]
pub trait GroupMetadata: Sync {
    fn insert_metadata(&self, idx: usize, key: String, value: String);
    fn clear_metadata(&self, idx: usize);
}

#[cfg(target_os = "linux")]
impl GroupMetadata for CounterGroup {
    fn insert_metadata(&self, idx: usize, key: String, value: String) {
        CounterGroup::insert_metadata(self, idx, key, value);
    }

    fn clear_metadata(&self, idx: usize) {
        CounterGroup::clear_metadata(self, idx);
    }
}

#[cfg(target_os = "linux")]
impl GroupMetadata for GaugeGroup {
    fn insert_metadata(&self, idx: usize, key: String, value: String) {
        GaugeGroup::insert_metadata(self, idx, key, value);
    }

    fn clear_metadata(&self, idx: usize) {
        GaugeGroup::clear_metadata(self, idx);
    }
}
