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

    pub fn len(&self) -> usize {
        self.entries
    }

    fn get_or_init(&self) -> &RwLock<Vec<i64>> {
        self.values
            .get_or_init(|| vec![i64::MIN; self.entries].into())
    }

    pub fn load_metadata(&self, idx: usize) -> Option<HashMap<String, String>> {
        match self.metadata.get() {
            Some(metadata) => metadata.read().get(idx).cloned(),
            None => None,
        }
    }

    pub fn clear_metadata(&self, idx: usize) {
        if let Some(metadata) = self.metadata.get() {
            let _ = metadata.write().get(idx).cloned();
        }
    }

    pub fn insert_metadata(&self, idx: usize, key: String, value: String) {
        let metadata = self
            .metadata
            .get_or_init(|| vec![HashMap::new(); self.entries].into());
        if let Some(metadata) = metadata.write().get_mut(idx) {
            metadata.insert(key, value);
        }
    }
}
