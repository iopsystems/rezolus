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

    /// Group by the specified labels, returning a map of label values to summed series
    pub fn group_by(
        &self,
        group_labels: &[String],
        filter: &Labels,
    ) -> Vec<(Vec<(String, String)>, UntypedSeries)> {
        let mut groups: HashMap<Vec<(String, String)>, UntypedSeries> = HashMap::new();

        for (labels, series) in self.inner.iter() {
            // Skip if doesn't match filter
            if !labels.matches(filter) {
                continue;
            }

            // Extract the values for the grouping labels
            let mut group_key = Vec::new();
            for label_name in group_labels {
                if let Some(label_value) = labels.inner.get(label_name) {
                    group_key.push((label_name.clone(), label_value.clone()));
                }
            }

            let untyped = series.untyped();

            // Add this series to the appropriate group
            if let Some(group_series) = groups.get_mut(&group_key) {
                // Add to existing group
                for (time, value) in untyped.inner.iter() {
                    if !group_series.inner.contains_key(time) {
                        group_series.inner.insert(*time, *value);
                    } else {
                        *group_series.inner.get_mut(time).unwrap() += value;
                    }
                }
            } else {
                // Create new group with this series
                groups.insert(group_key, untyped);
            }
        }

        groups.into_iter().collect()
    }
}
