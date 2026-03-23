use crate::agent::metrics::MetricGroup;
use metriken::Metric;
use metriken::Value;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::OnceLock;
use thiserror::Error;

type OnceLockVec<T> = OnceLock<RwLock<Vec<T>>>;

/// A pointer to a memory-mapped u64 slice from a BPF map.
///
/// This allows reading counter values directly from the kernel's BPF map
/// without copying into a separate userspace buffer. The backing mmap lives
/// for the process lifetime (BPF maps are never unmapped).
///
/// # Safety
/// The pointer must remain valid for the lifetime of the process. This is
/// guaranteed because BPF map mmaps are created at sampler init and never
/// unmapped. `MmapMut` (from memmap2) is `Send + Sync`.
pub struct MmapSlice {
    ptr: *const u64,
    len: usize,
}

unsafe impl Send for MmapSlice {}
unsafe impl Sync for MmapSlice {}

impl MmapSlice {
    /// Create a new MmapSlice from a raw pointer and length.
    ///
    /// # Safety
    /// The caller must ensure that `ptr` points to a valid, aligned slice of
    /// `len` u64 values that remains valid for the lifetime of this struct.
    pub unsafe fn new(ptr: *const u64, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Return the mmap contents as a u64 slice.
    pub fn as_slice(&self) -> &[u64] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

#[derive(Error, Debug, PartialEq)]
#[allow(dead_code)]
pub enum CounterGroupError {
    #[error("the index is higher than the counter group size")]
    InvalidIndex,
}

/// A group of counters that's protected by a reader-writer lock.
#[allow(dead_code)]
pub struct CounterGroup {
    values: OnceLockVec<u64>,
    mmap_values: OnceLock<MmapSlice>,
    metadata: OnceLockVec<HashMap<String, String>>,
    entries: usize,
}

impl Metric for CounterGroup {
    fn as_any(&self) -> std::option::Option<&(dyn std::any::Any + 'static)> {
        Some(self)
    }

    fn value(&self) -> std::option::Option<metriken::Value<'_>> {
        Some(Value::Other(self))
    }
}

#[allow(dead_code)]
impl CounterGroup {
    /// Create a new counter group
    pub const fn new(entries: usize) -> Self {
        Self {
            values: OnceLock::new(),
            mmap_values: OnceLock::new(),
            metadata: OnceLock::new(),
            entries,
        }
    }

    /// Attach a memory-mapped BPF map as the backing store for counter values.
    ///
    /// When attached, `load_values()` returns a zero-copy slice from the mmap
    /// and `PackedCounters::refresh()` becomes a no-op for values.
    ///
    /// # Safety
    /// The caller must ensure that `ptr` points to a valid, aligned slice of
    /// `len` u64 values backed by a BPF map mmap that lives for the process
    /// lifetime.
    pub unsafe fn attach_mmap_values(&self, ptr: *const u64, len: usize) {
        let _ = self.mmap_values.set(MmapSlice::new(ptr, len));
    }

    /// Load counter values as a zero-copy slice from the attached BPF mmap.
    ///
    /// Returns `None` if no mmap is attached (use `load()` as fallback).
    pub fn load_values(&self) -> Option<&[u64]> {
        self.mmap_values.get().map(|m| m.as_slice())
    }

    /// Sets the counter at a given index to the provided value
    pub fn set(&self, idx: usize, value: u64) -> Result<(), CounterGroupError> {
        if idx >= self.entries {
            Err(CounterGroupError::InvalidIndex)
        } else {
            let mut inner = self.get_or_init().write();

            inner[idx] = value;

            Ok(())
        }
    }

    /// Load the counter values (allocates a clone). Prefer `load_values()` when
    /// an mmap is attached for zero-copy access.
    pub fn load(&self) -> Option<Vec<u64>> {
        self.values.get().map(|v| v.read().clone())
    }

    fn get_or_init(&self) -> &RwLock<Vec<u64>> {
        self.values.get_or_init(|| vec![0; self.entries].into())
    }
}

impl MetricGroup for CounterGroup {
    fn insert_metadata(&self, idx: usize, key: String, value: String) {
        let metadata = self
            .metadata
            .get_or_init(|| vec![HashMap::new(); self.entries].into());
        if let Some(metadata) = metadata.write().get_mut(idx) {
            metadata.insert(key, value);
        }
    }

    fn load_metadata(&self, idx: usize) -> Option<HashMap<String, String>> {
        match self.metadata.get() {
            Some(metadata) => metadata.read().get(idx).cloned(),
            None => None,
        }
    }

    fn clear_metadata(&self, idx: usize) {
        if let Some(metadata) = self.metadata.get() {
            if let Some(m) = metadata.write().get_mut(idx) {
                m.clear();
                m.shrink_to_fit();
            }
        }
    }

    fn len(&self) -> usize {
        self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_load() {
        let group = CounterGroup::new(4);
        assert!(group.load().is_none());

        group.set(0, 100).unwrap();
        group.set(3, 400).unwrap();

        let values = group.load().unwrap();
        assert_eq!(values, vec![100, 0, 0, 400]);
    }

    #[test]
    fn set_out_of_bounds() {
        let group = CounterGroup::new(4);
        assert_eq!(group.set(4, 1), Err(CounterGroupError::InvalidIndex));
        assert_eq!(group.set(100, 1), Err(CounterGroupError::InvalidIndex));
    }

    #[test]
    fn overwrite_value() {
        let group = CounterGroup::new(2);
        group.set(0, 10).unwrap();
        group.set(0, 20).unwrap();
        assert_eq!(group.load().unwrap()[0], 20);
    }

    #[test]
    fn metadata_lifecycle() {
        let group = CounterGroup::new(4);

        // no metadata before any insert
        assert!(group.load_metadata(0).is_none());

        // insert metadata
        group.insert_metadata(0, "key".into(), "value".into());
        let m = group.load_metadata(0).unwrap();
        assert_eq!(m.get("key").unwrap(), "value");

        // other indices are empty
        let m = group.load_metadata(1).unwrap();
        assert!(m.is_empty());

        // clear metadata
        group.clear_metadata(0);
        let m = group.load_metadata(0).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn metadata_multiple_keys() {
        let group = CounterGroup::new(2);
        group.insert_metadata(0, "a".into(), "1".into());
        group.insert_metadata(0, "b".into(), "2".into());

        let m = group.load_metadata(0).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a").unwrap(), "1");
        assert_eq!(m.get("b").unwrap(), "2");
    }

    #[test]
    fn metadata_out_of_bounds_is_ignored() {
        let group = CounterGroup::new(2);
        // should not panic
        group.insert_metadata(10, "key".into(), "value".into());
        assert!(group.load_metadata(10).is_none());
    }

    #[test]
    fn clear_metadata_before_init_is_noop() {
        let group = CounterGroup::new(4);
        // should not panic when metadata OnceLock hasn't been initialized
        group.clear_metadata(0);
    }

    #[test]
    fn len() {
        let group = CounterGroup::new(128);
        assert_eq!(group.len(), 128);
    }

    #[test]
    fn load_values_without_mmap() {
        let group = CounterGroup::new(4);
        assert!(group.load_values().is_none());
    }

    #[test]
    fn load_values_from_mmap() {
        let group = CounterGroup::new(4);
        let data: Vec<u64> = vec![10, 20, 0, 40];

        unsafe {
            group.attach_mmap_values(data.as_ptr(), data.len());
        }

        let values = group.load_values().unwrap();
        assert_eq!(values, &[10, 20, 0, 40]);

        // load() should still return None (values Vec was never initialized)
        assert!(group.load().is_none());

        // Keep data alive for the duration of the test
        drop(data);
    }
}
