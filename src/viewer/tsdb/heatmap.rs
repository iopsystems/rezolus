use super::*;

#[derive(Default, Clone)]
pub struct Heatmap {
    inner: BTreeMap<usize, UntypedSeries>,
}

#[derive(Serialize)]
pub struct HeatmapData {
    pub time: Vec<f64>,
    pub formatted_time: Vec<String>,
    pub data: Vec<Vec<f64>>,
    pub min_value: f64,
    pub max_value: f64,
}

impl Heatmap {
    pub fn as_data(&self) -> HeatmapData {
        let mut timestamps = BTreeSet::new();
        let mut min_value = f64::MAX;
        let mut max_value = f64::MIN;

        // Get the maximum CPU ID
        let max_cpu_id = if self.inner.is_empty() {
            0
        } else {
            *self.inner.keys().max().unwrap_or(&0)
        };

        // Collect unique timestamps and find min/max values
        for series in self.inner.values() {
            for (ts, value) in series.inner.iter() {
                timestamps.insert(*ts);
                min_value = min_value.min(*value);
                max_value = max_value.max(*value);
            }
        }

        let timestamps: Vec<u64> = timestamps.into_iter().collect();

        // Convert to seconds and pre-format
        let timestamp_seconds: Vec<f64> = timestamps
            .iter()
            .map(|ts| *ts as f64 / 1000000000.0)
            .collect();

        let formatted_times: Vec<String> = timestamps
            .iter()
            .map(|ts| {
                let unix_seconds = *ts as f64 / 1000000000.0;
                format_timestamp(unix_seconds)
            })
            .collect();

        // Create a hash map for timestamps for faster lookups
        let mut timestamp_indices = HashMap::with_capacity(timestamps.len());
        for (i, ts) in timestamps.iter().enumerate() {
            timestamp_indices.insert(ts, i);
        }

        // OPTIMIZATION: Organize data by CPU ID (y-axis value) first
        // This makes tooltip lookups much faster since ECharts can directly
        // access the relevant row where the mouse is hovering
        let mut heatmap_data = Vec::with_capacity(timestamps.len() * (max_cpu_id + 1));

        // Sort by CPU ID (y-axis value) first, then by timestamp (x-axis value)
        // This organizes data points in rows which match how the heatmap is rendered
        for cpu_id in 0..=max_cpu_id {
            if let Some(series) = self.inner.get(&cpu_id) {
                for (ts, value) in series.inner.iter() {
                    if let Some(&i) = timestamp_indices.get(&ts) {
                        heatmap_data.push(vec![i as f64, cpu_id as f64, *value]);
                    }
                }
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
            formatted_time: formatted_times,
            data: heatmap_data,
            min_value,
            max_value,
        }
    }
}

// Helper function to format timestamps consistently
fn format_timestamp(unix_seconds: f64) -> String {
    // We could add an external date formatting library here,
    // but for now we'll use a simple approach

    // Convert to milliseconds for consistent handling
    let ms = (unix_seconds * 1000.0) as i64;

    // Extract time components
    let seconds = (ms / 1000) % 60;
    let minutes = (ms / (1000 * 60)) % 60;
    let hours = (ms / (1000 * 60 * 60)) % 24;

    // Format time as HH:MM:SS
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

impl Div<Heatmap> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: Heatmap) -> Self::Output {
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
    fn mul(self, other: Heatmap) -> Self::Output {
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

impl Div<UntypedSeries> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: UntypedSeries) -> Self::Output {
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
    fn div(self, other: f64) -> Self::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other);
        }

        result
    }
}
