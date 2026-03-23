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
pub enum GaugeGroupError {
    #[error("the index is higher than the gauge group size")]
    InvalidIndex,
}

/// A group of counters that's protected by a reader-writer lock.
#[allow(dead_code)]
pub struct GaugeGroup {
    values: OnceLockVec<i64>,
    metadata: OnceLockVec<HashMap<String, String>>,
    entries: usize,
}

impl Metric for GaugeGroup {
    fn as_any(&self) -> std::option::Option<&(dyn std::any::Any + 'static)> {
        Some(self)
    }

    fn value(&self) -> std::option::Option<metriken::Value<'_>> {
        Some(Value::Other(self))
    }
}

#[allow(dead_code)]
impl GaugeGroup {
    /// Create a new counter group
    pub const fn new(entries: usize) -> Self {
        Self {
            values: OnceLock::new(),
            metadata: OnceLock::new(),
            entries,
        }
    }

    /// Sets the counter at a given index to the provided value
    pub fn set(&self, idx: usize, value: i64) -> Result<(), GaugeGroupError> {
        if idx >= self.entries {
            Err(GaugeGroupError::InvalidIndex)
        } else {
            let mut inner = self.get_or_init().write();

            inner[idx] = value;

            Ok(())
        }
    }

    /// Load the counter values
    pub fn load(&self) -> Option<Vec<i64>> {
        self.values.get().map(|v| v.read().clone())
    }

    fn get_or_init(&self) -> &RwLock<Vec<i64>> {
        self.values
            .get_or_init(|| vec![i64::MIN; self.entries].into())
    }
}

impl MetricGroup for GaugeGroup {
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
        let group = GaugeGroup::new(4);
        assert!(group.load().is_none());

        group.set(0, 100).unwrap();
        group.set(3, -50).unwrap();

        let values = group.load().unwrap();
        assert_eq!(values[0], 100);
        assert_eq!(values[1], i64::MIN); // unset slots are i64::MIN
        assert_eq!(values[2], i64::MIN);
        assert_eq!(values[3], -50);
    }

    #[test]
    fn set_out_of_bounds() {
        let group = GaugeGroup::new(4);
        assert_eq!(group.set(4, 1), Err(GaugeGroupError::InvalidIndex));
        assert_eq!(group.set(100, 1), Err(GaugeGroupError::InvalidIndex));
    }

    #[test]
    fn overwrite_value() {
        let group = GaugeGroup::new(2);
        group.set(0, 10).unwrap();
        group.set(0, 20).unwrap();
        assert_eq!(group.load().unwrap()[0], 20);
    }

    #[test]
    fn metadata_lifecycle() {
        let group = GaugeGroup::new(4);

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
        let group = GaugeGroup::new(2);
        group.insert_metadata(0, "a".into(), "1".into());
        group.insert_metadata(0, "b".into(), "2".into());

        let m = group.load_metadata(0).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a").unwrap(), "1");
        assert_eq!(m.get("b").unwrap(), "2");
    }

    #[test]
    fn metadata_out_of_bounds_is_ignored() {
        let group = GaugeGroup::new(2);
        // should not panic
        group.insert_metadata(10, "key".into(), "value".into());
        assert!(group.load_metadata(10).is_none());
    }

    #[test]
    fn clear_metadata_before_init_is_noop() {
        let group = GaugeGroup::new(4);
        // should not panic when metadata OnceLock hasn't been initialized
        group.clear_metadata(0);
    }

    #[test]
    fn negative_values() {
        let group = GaugeGroup::new(2);
        group.set(0, -1000).unwrap();
        group.set(1, i64::MAX).unwrap();

        let values = group.load().unwrap();
        assert_eq!(values[0], -1000);
        assert_eq!(values[1], i64::MAX);
    }

    #[test]
    fn len() {
        let group = GaugeGroup::new(64);
        assert_eq!(group.len(), 64);
    }
}
