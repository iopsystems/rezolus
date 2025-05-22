use super::*;

/// Represents a series of counter readings.
#[derive(Default, Clone)]
pub struct HistogramSeries {
    inner: BTreeMap<u64, Histogram>,
}

impl HistogramSeries {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&mut self, timestamp: u64, value: Histogram) {
        self.inner.insert(timestamp, value);
    }

    pub fn percentiles(&self, percentiles: &[f64]) -> Option<Vec<UntypedSeries>> {
        if self.is_empty() {
            return None;
        }

        let (_, mut prev) = self.inner.first_key_value().unwrap();

        let mut result = vec![UntypedSeries::default(); percentiles.len()];

        for (time, curr) in self.inner.iter().skip(1) {
            let delta = curr.wrapping_sub(prev).unwrap();

            if let Ok(Some(percentiles)) = delta.percentiles(percentiles) {
                for (id, (_, bucket)) in percentiles.iter().enumerate() {
                    result[id].inner.insert(*time, bucket.end() as f64);
                }
            }

            prev = curr;
        }

        Some(result)
    }
}

impl Add<&HistogramSeries> for HistogramSeries {
    type Output = HistogramSeries;
    fn add(self, other: &HistogramSeries) -> Self::Output {
        let mut result = self.clone();

        for (time, histogram) in other.inner.iter() {
            if let Some(h) = result.inner.get_mut(time) {
                *h = h.wrapping_add(histogram).expect("histogram mismatch");
            } else {
                result.inner.insert(*time, histogram.clone());
            }
        }

        result
    }
}
