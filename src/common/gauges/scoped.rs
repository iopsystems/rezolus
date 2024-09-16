use crate::common::DynamicGauge;

pub struct ScopedGauges {
    inner: Vec<Vec<DynamicGauge>>,
}

impl ScopedGauges {
    /// Create a new bank of gauges.
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// Sets the gauge at the provided index to a new value. Returns the
    /// previous value.
    pub fn set(&self, scope: usize, idx: usize, value: i64) -> Option<i64> {
        match self.inner.get(scope).map(|scope| scope.get(idx)) {
            Some(Some(c)) => Some(c.set(value)),
            _ => None,
        }
    }

    pub fn push(&mut self, scope: usize, gauge: DynamicGauge) {
        self.inner.resize_with(scope + 1, Default::default);
        self.inner[scope].push(gauge);
    }
}
