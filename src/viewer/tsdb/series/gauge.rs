use super::*;

/// Represents a series of gauge readings.
#[derive(Default, Clone)]
pub struct GaugeSeries {
    inner: BTreeMap<u64, i64>,
}

impl GaugeSeries {
    pub fn insert(&mut self, timestamp: u64, value: i64) {
        self.inner.insert(timestamp, value);
    }

    pub fn untyped(&self) -> UntypedSeries {
        UntypedSeries {
            inner: self.inner.iter().map(|(k, v)| (*k, *v as f64)).collect(),
        }
    }
}
