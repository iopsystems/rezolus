use super::*;

/// Represents a collection of counter timeseries keyed on label sets.
#[derive(Default)]
pub struct CounterCollection {
    inner: HashMap<Labels, CounterSeries>,
}

impl CounterCollection {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, CounterSeries> {
        self.inner.entry(labels)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Labels, &CounterSeries)> {
        self.inner.iter()
    }

    /// Old filter method that clones - kept for compatibility but should be avoided
    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    /// Efficiently compute rate for all series
    pub fn rate(&self) -> UntypedCollection {
        let mut result = UntypedCollection::default();

        for (labels, series) in self.inner.iter() {
            result.insert(labels.clone(), series.rate());
        }

        result
    }

    /// Efficiently compute rate only for series matching the filter
    /// This avoids cloning the time series data
    pub fn filtered_rate(&self, filter: &Labels) -> UntypedCollection {
        let mut result = UntypedCollection::default();

        for (labels, series) in self.inner.iter() {
            if labels.matches(filter) {
                result.insert(labels.clone(), series.rate());
            }
        }

        result
    }
}
