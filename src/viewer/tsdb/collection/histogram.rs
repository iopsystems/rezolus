use super::*;

/// Represents a collection of histogram timeseries keyed on label sets.
#[derive(Default)]
pub struct HistogramCollection {
    inner: HashMap<Labels, HistogramSeries>,
}

impl HistogramCollection {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, HistogramSeries> {
        self.inner.entry(labels)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Labels, &HistogramSeries)> {
        self.inner.iter()
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

    pub fn sum(&self) -> HistogramSeries {
        let mut result = HistogramSeries::default();

        for series in self.inner.values() {
            result = result + series;
        }

        result
    }
}
