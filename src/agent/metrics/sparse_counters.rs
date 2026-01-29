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
