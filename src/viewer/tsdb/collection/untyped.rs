use super::*;

#[derive(Default)]
pub struct UntypedCollection {
    inner: HashMap<Labels, UntypedSeries>,
}

impl UntypedCollection {
    pub fn insert(&mut self, labels: Labels, series: UntypedSeries) {
        self.inner.insert(labels, series);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Labels, &UntypedSeries)> {
        self.inner.iter()
    }

    pub fn sum(&self) -> UntypedSeries {
        let mut result = UntypedSeries::default();

        for series in self.inner.values() {
            for (time, value) in series.inner.iter() {
                if !result.inner.contains_key(time) {
                    result.inner.insert(*time, *value);
                } else {
                    *result.inner.get_mut(time).unwrap() += value;
                }
            }
        }

        result
    }

    /// Group by the specified labels, returning a map of label values to summed series
    pub fn group_by(&self, group_labels: &[String]) -> Vec<(Vec<(String, String)>, UntypedSeries)> {
        let mut groups: HashMap<Vec<(String, String)>, UntypedSeries> = HashMap::new();

        for (labels, series) in self.inner.iter() {
            // Extract the values for the grouping labels
            let mut group_key = Vec::new();
            for label_name in group_labels {
                if let Some(label_value) = labels.inner.get(label_name) {
                    group_key.push((label_name.clone(), label_value.clone()));
                }
            }

            // Add this series to the appropriate group
            if let Some(group_series) = groups.get_mut(&group_key) {
                // Add to existing group
                for (time, value) in series.inner.iter() {
                    if !group_series.inner.contains_key(time) {
                        group_series.inner.insert(*time, *value);
                    } else {
                        *group_series.inner.get_mut(time).unwrap() += value;
                    }
                }
            } else {
                // Create new group with this series
                groups.insert(group_key, series.clone());
            }
        }

        groups.into_iter().collect()
    }
}
