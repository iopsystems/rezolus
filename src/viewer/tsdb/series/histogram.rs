use super::*;

/// Represents a series of histogram readings.
#[derive(Default, Clone)]
pub struct HistogramSeries {
    inner: BTreeMap<u64, Histogram>,
}

/// Data for rendering a histogram as a latency heatmap
#[derive(Default, Clone)]
pub struct HistogramHeatmapData {
    /// Timestamps in seconds
    pub timestamps: Vec<f64>,
    /// Bucket boundaries (end values) for Y-axis labels
    pub bucket_bounds: Vec<u64>,
    /// Heatmap data as [time_index, bucket_index, count]
    pub data: Vec<(usize, usize, f64)>,
    /// Minimum count value (for color scaling)
    pub min_value: f64,
    /// Maximum count value (for color scaling)
    pub max_value: f64,
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

    /// Returns bucket data suitable for rendering as a heatmap.
    /// Y-axis is bucket index (latency range), X-axis is time, color is count.
    pub fn heatmap(&self) -> Option<HistogramHeatmapData> {
        if self.inner.len() < 2 {
            return None;
        }

        let mut result = HistogramHeatmapData::default();
        let mut min_value = f64::MAX;
        let mut max_value = f64::MIN;

        let (_, mut prev) = self.inner.first_key_value().unwrap();

        // Collect bucket boundaries from the first histogram
        // (all histograms in the series should have the same config)
        let mut bucket_bounds_set = false;

        for (time, curr) in self.inner.iter().skip(1) {
            let delta = curr.wrapping_sub(prev).unwrap();
            let time_index = result.timestamps.len();

            // Store timestamp in seconds
            result.timestamps.push(*time as f64 / 1_000_000_000.0);

            // Iterate over buckets and collect counts
            for (bucket_index, bucket) in delta.iter().enumerate() {
                let count = bucket.count();

                // Only include non-zero buckets to save space
                if count > 0 {
                    let count_f64 = count as f64;
                    result.data.push((time_index, bucket_index, count_f64));
                    min_value = min_value.min(count_f64);
                    max_value = max_value.max(count_f64);
                }

                // Collect bucket boundaries once
                if !bucket_bounds_set {
                    result.bucket_bounds.push(bucket.end());
                }
            }

            bucket_bounds_set = true;
            prev = curr;
        }

        // Handle edge cases
        if min_value == f64::MAX {
            min_value = 0.0;
        }
        if max_value == f64::MIN {
            max_value = 0.0;
        }

        result.min_value = min_value;
        result.max_value = max_value;

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
