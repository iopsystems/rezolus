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

    pub fn iter(&self) -> impl Iterator<Item = (&Labels, &GaugeSeries)> {
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

    /// Efficiently compute sum only for series matching the filter
    /// This avoids cloning the time series data
    pub fn filtered_sum(&self, filter: &Labels) -> UntypedSeries {
        let mut result = UntypedSeries::default();

        for (labels, series) in self.inner.iter() {
            if labels.matches(filter) {
                let untyped = series.untyped();
                for (time, value) in untyped.inner.iter() {
                    if result.inner.contains_key(time) {
                        *result.inner.get_mut(time).unwrap() += value;
                    } else {
                        result.inner.insert(*time, *value);
                    }
                }
            }
        }

        result
    }
}
