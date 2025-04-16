use super::*;

/// Represents a collection of gauge timeseries keyed on label sets.
#[derive(Default)]
pub struct GaugeCollection {
    inner: HashMap<Labels, GaugeSeries>,
}

impl GaugeCollection {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, GaugeSeries> {
        self.inner.entry(labels)
    }

    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    /// Convert the gauge collection to a untyped collection by converting `i64`
    /// gauges to `f64` representation.
    pub fn untyped(&self) -> UntypedCollection {
        let mut result = UntypedCollection::default();

        for (labels, series) in self.inner.iter() {
            result.insert(labels.clone(), series.untyped());
        }

        result
    }

    /// Convenience function to sum all gauges in the collection into a single
    /// `UntypedSeries`
    pub fn sum(&self) -> UntypedSeries {
        self.untyped().sum()
    }
}