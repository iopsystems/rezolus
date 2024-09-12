use crate::common::DynamicCounter;

pub struct ScopedCounters {
    inner: Vec<Vec<DynamicCounter>>,
}

impl ScopedCounters {
    /// Create a new bank of scoped counters.
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Sets the counter for a scope and index to a new value. Returns the
    /// previous value.
    pub fn set(&self, scope: usize, idx: usize, value: u64) -> Option<u64> {
        match self.inner.get(scope).map(|scope| scope.get(idx)) {
            Some(Some(c)) => Some(c.set(value)),
            _ => None,
        }
    }

    pub fn add(&self, scope: usize, idx: usize, value: u64) -> Option<u64> {
        match self.inner.get(scope).map(|scope| scope.get(idx)) {
            Some(Some(c)) => Some(c.add(value)),
            _ => None,
        }
    }

    pub fn push(&mut self, scope: usize, counter: DynamicCounter) {
        self.inner.resize_with(scope + 1, Default::default);
        self.inner[scope].push(counter);
    }
}
