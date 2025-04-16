use super::*;

#[derive(Default, Clone)]
pub struct Heatmap {
    inner: BTreeMap<usize, Timeseries>,
}

#[derive(Serialize)]
pub struct HeatmapData {
    pub time: Vec<f64>,
    pub data: Vec<Vec<f64>>,
    pub min_value: f64,
    pub max_value: f64,
}

impl Heatmap {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&mut self, id: usize, series: Timeseries) {
        self.inner.insert(id, series);
    }

    pub fn as_data(&self) -> Vec<Vec<f64>> {
        let mut timestamps = BTreeSet::new();

        for series in self.inner.values() {
            for ts in series.inner.keys() {
                timestamps.insert(ts);
            }
        }

        let timestamps: Vec<u64> = timestamps.into_iter().copied().collect();

        let mut data = Vec::new();

        data.push(
            timestamps
                .iter()
                .map(|v| *v as f64 / 1000000000.0)
                .collect(),
        );

        for (id, series) in self.inner.iter() {
            let id = id + 1;

            data.resize_with(id + 1, Vec::new);

            if let Some((start, mut prev)) = series.inner.first_key_value() {
                for ts in &timestamps {
                    if ts < start {
                        data[id].push(0.0);
                    } else if let Some(v) = series.inner.get(ts) {
                        data[id].push(*v);
                        prev = v;
                    } else {
                        data[id].push(*prev);
                    }
                }
            }
        }

        data
    }

    // New method to return data in a format directly consumable by ECharts
    pub fn as_echarts_data(&self) -> HeatmapData {
        let mut timestamps = BTreeSet::new();
        let mut min_value = f64::MAX;
        let mut max_value = f64::MIN;

        // Find the maximum CPU ID to determine the range
        let max_cpu_id = if self.inner.is_empty() {
            0
        } else {
            *self.inner.keys().max().unwrap_or(&0)
        };

        // Collect all timestamps and find min/max values
        for series in self.inner.values() {
            for (ts, value) in series.inner.iter() {
                timestamps.insert(*ts);
                min_value = min_value.min(*value);
                max_value = max_value.max(*value);
            }
        }

        let timestamps: Vec<u64> = timestamps.into_iter().collect();

        // Pre-format timestamps to unix seconds
        let timestamp_seconds: Vec<f64> = timestamps
            .iter()
            .map(|ts| *ts as f64 / 1000000000.0)
            .collect();

        // Convert to ECharts format: [[x, y, value], ...]
        let mut heatmap_data = Vec::new();

        // Make sure we have data for all CPUs from 0 to max_cpu_id
        for cpu_id in 0..=max_cpu_id {
            let series = self.inner.get(&cpu_id);

            for (i, ts) in timestamps.iter().enumerate() {
                // If we have data for this CPU and timestamp, use it
                // Otherwise, use zero
                let value = if let Some(cpu_series) = series {
                    cpu_series.inner.get(ts).copied().unwrap_or(0.0)
                } else {
                    0.0
                };

                // Add the data point: [timestamp index, CPU ID, value]
                heatmap_data.push(vec![i as f64, cpu_id as f64, value]);
            }
        }

        // Ensure min_value and max_value are meaningful
        if min_value == f64::MAX {
            min_value = 0.0;
        }
        if max_value == f64::MIN {
            max_value = 0.0;
        }

        HeatmapData {
            time: timestamp_seconds,
            data: heatmap_data,
            min_value,
            max_value,
        }
    }
}

impl Div<Heatmap> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: Heatmap) -> <Self as Div<Timeseries>>::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, this)) = this.pop_first() {
            if let Some(other) = other.inner.get(&id) {
                result.inner.insert(id, this / other);
            }
        }

        result
    }
}

impl Mul<Heatmap> for Heatmap {
    type Output = Heatmap;
    fn mul(self, other: Heatmap) -> <Self as Div<Timeseries>>::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, this)) = this.pop_first() {
            if let Some(other) = other.inner.get(&id) {
                result.inner.insert(id, this * other);
            }
        }

        result
    }
}

impl Div<Timeseries> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: Timeseries) -> <Self as Div<Timeseries>>::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other.clone());
        }

        result
    }
}

impl Div<f64> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: f64) -> <Self as Div<Timeseries>>::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other);
        }

        result
    }
}
