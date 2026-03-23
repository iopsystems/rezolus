use crate::agent::metrics::MetricGroup;
use metriken::Metric;
use metriken::Value;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::OnceLock;
use thiserror::Error;

type OnceLockVec<T> = OnceLock<RwLock<Vec<T>>>;

#[derive(Error, Debug, PartialEq)]
#[allow(dead_code)]
pub enum SparseCounterGroupError {
    #[error("the index is higher than the counter group size")]
    InvalidIndex,
}

/// A group of counters with sparse metadata storage.
///
/// Unlike `CounterGroup`, this type uses a HashMap for metadata storage,
/// making it suitable for high-cardinality metrics (like per-task) where
/// most indices will not have metadata at any given time.
///
/// The counter values are still stored densely (for BPF mmap compatibility),
/// but metadata only consumes memory for active entries.
#[allow(dead_code)]
pub struct SparseCounterGroup {
    values: OnceLockVec<u64>,
    metadata: OnceLock<RwLock<HashMap<usize, HashMap<String, String>>>>,
    entries: usize,
}

impl Metric for SparseCounterGroup {
    fn as_any(&self) -> std::option::Option<&(dyn std::any::Any + 'static)> {
        Some(self)
    }

    fn value(&self) -> std::option::Option<metriken::Value<'_>> {
        Some(Value::Other(self))
    }
}

#[allow(dead_code)]
impl SparseCounterGroup {
    /// Create a new sparse counter group
    pub const fn new(entries: usize) -> Self {
        Self {
            values: OnceLock::new(),
            metadata: OnceLock::new(),
            entries,
        }
    }

    /// Sets the counter at a given index to the provided value
    pub fn set(&self, idx: usize, value: u64) -> Result<(), SparseCounterGroupError> {
        if idx >= self.entries {
            Err(SparseCounterGroupError::InvalidIndex)
        } else {
            let mut inner = self.get_or_init().write();

            inner[idx] = value;

            Ok(())
        }
    }

    /// Load the counter values
    pub fn load(&self) -> Option<Vec<u64>> {
        self.values.get().map(|v| v.read().clone())
    }

    fn get_or_init(&self) -> &RwLock<Vec<u64>> {
        self.values.get_or_init(|| vec![0; self.entries].into())
    }
}

impl MetricGroup for SparseCounterGroup {
    fn insert_metadata(&self, idx: usize, key: String, value: String) {
        if idx >= self.entries {
            return;
        }
        let metadata = self.metadata.get_or_init(|| RwLock::new(HashMap::new()));
        metadata.write().entry(idx).or_default().insert(key, value);
    }

    fn load_metadata(&self, idx: usize) -> Option<HashMap<String, String>> {
        self.metadata
            .get()
            .and_then(|m| m.read().get(&idx).cloned())
    }

    fn clear_metadata(&self, idx: usize) {
        if let Some(metadata) = self.metadata.get() {
            metadata.write().remove(&idx);
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
        let group = SparseCounterGroup::new(4);
        assert!(group.load().is_none());

        group.set(0, 100).unwrap();
        group.set(3, 400).unwrap();

        let values = group.load().unwrap();
        assert_eq!(values, vec![100, 0, 0, 400]);
    }

    #[test]
    fn set_out_of_bounds() {
        let group = SparseCounterGroup::new(4);
        assert_eq!(group.set(4, 1), Err(SparseCounterGroupError::InvalidIndex));
        assert_eq!(
            group.set(100, 1),
            Err(SparseCounterGroupError::InvalidIndex)
        );
    }

    #[test]
    fn overwrite_value() {
        let group = SparseCounterGroup::new(2);
        group.set(0, 10).unwrap();
        group.set(0, 20).unwrap();
        assert_eq!(group.load().unwrap()[0], 20);
    }

    #[test]
    fn metadata_lifecycle() {
        let group = SparseCounterGroup::new(4);

        // no metadata before any insert
        assert!(group.load_metadata(0).is_none());

        // insert metadata
        group.insert_metadata(0, "key".into(), "value".into());
        let m = group.load_metadata(0).unwrap();
        assert_eq!(m.get("key").unwrap(), "value");

        // other indices have no metadata (sparse: not even present)
        assert!(group.load_metadata(1).is_none());

        // clear metadata actually removes the entry
        group.clear_metadata(0);
        assert!(group.load_metadata(0).is_none());
    }

    #[test]
    fn metadata_multiple_keys() {
        let group = SparseCounterGroup::new(2);
        group.insert_metadata(0, "a".into(), "1".into());
        group.insert_metadata(0, "b".into(), "2".into());

        let m = group.load_metadata(0).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a").unwrap(), "1");
        assert_eq!(m.get("b").unwrap(), "2");
    }

    #[test]
    fn metadata_out_of_bounds_is_ignored() {
        let group = SparseCounterGroup::new(2);
        // should not panic
        group.insert_metadata(10, "key".into(), "value".into());
        assert!(group.load_metadata(10).is_none());
    }

    #[test]
    fn clear_metadata_before_init_is_noop() {
        let group = SparseCounterGroup::new(4);
        // should not panic when metadata OnceLock hasn't been initialized
        group.clear_metadata(0);
    }

    #[test]
    fn clear_metadata_fully_removes_entry() {
        let group = SparseCounterGroup::new(8);
        group.insert_metadata(3, "pid".into(), "42".into());
        group.insert_metadata(3, "comm".into(), "nginx".into());

        assert!(group.load_metadata(3).is_some());

        group.clear_metadata(3);

        // after clear, the entry should be completely gone (not just empty)
        assert!(group.load_metadata(3).is_none());
    }

    #[test]
    fn metadata_reuse_after_clear() {
        let group = SparseCounterGroup::new(8);

        // simulate task lifecycle: create, exit, new task reuses PID
        group.insert_metadata(5, "comm".into(), "old_task".into());
        group.clear_metadata(5);
        group.insert_metadata(5, "comm".into(), "new_task".into());

        let m = group.load_metadata(5).unwrap();
        assert_eq!(m.get("comm").unwrap(), "new_task");
        assert_eq!(m.len(), 1); // only the new metadata, no leftovers
    }

    #[test]
    fn len() {
        let group = SparseCounterGroup::new(128);
        assert_eq!(group.len(), 128);
    }
}
