use crate::viewer::PERCENTILES;
use arrow::array::Int64Array;
use arrow::array::ListArray;
use arrow::array::UInt64Array;
use arrow::datatypes::DataType;
use histogram::Histogram;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::num::ParseIntError;
use std::ops::*;
use std::path::Path;

#[derive(Default)]
pub struct Tsdb {
    inner: HashMap<String, TimeSeriesCollection>,
}

impl Tsdb {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut data = Tsdb::default();

        let file = File::open(path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;

        for batch in reader.into_iter().flatten() {
            let schema = batch.schema().clone();

            // row to timestamp in seconds
            let mut timestamps: BTreeMap<usize, u64> = BTreeMap::new();

            // loop to find the timestamp column, convert it to seconds, and
            // store it in the map
            for (id, field) in schema.fields().iter().enumerate() {
                if field.name() == "timestamp" {
                    let column = batch.column(id);

                    if *column.data_type() != DataType::UInt64 {
                        panic!("invalid timestamp column data type");
                    }

                    let values = column
                        .as_any()
                        .downcast_ref::<UInt64Array>()
                        .expect("Failed to downcast");

                    for (id, value) in values.iter().enumerate() {
                        if let Some(v) = value {
                            timestamps.insert(id, v);
                        }
                    }

                    break;
                }
            }

            // loop through all non-timestamp columns, and insert them into the
            // tsdb
            for (id, field) in schema.fields().iter().enumerate() {
                if field.name() == "timestamp" {
                    continue;
                }

                let mut meta = field.metadata().clone();

                let name = if let Some(n) = meta.get("metric").cloned() {
                    n
                } else {
                    continue;
                };

                let grouping_power: Option<Result<u8, ParseIntError>> =
                    meta.remove("grouping_power").map(|v| v.parse());

                let max_value_power: Option<Result<u8, ParseIntError>> =
                    meta.remove("max_value_power").map(|v| v.parse());

                let mut labels = Labels::default();

                for (k, v) in meta.iter() {
                    labels.inner.insert(k.to_string(), v.to_string());
                }

                let collection = data.inner.entry(name.to_string()).or_default();
                let series = collection.inner.entry(labels).or_default();

                let column = batch.column(id);

                match column.data_type() {
                    DataType::UInt64 => {
                        let values = column
                            .as_any()
                            .downcast_ref::<UInt64Array>()
                            .expect("Failed to downcast");

                        for (id, value) in values.iter().enumerate() {
                            if let Some(v) = value {
                                if let Some(ts) = timestamps.get(&id) {
                                    series.inner.insert(*ts, Value::Counter(v));
                                }
                            }
                        }
                    }
                    DataType::Int64 => {
                        let values = column
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .expect("Failed to downcast");

                        for (id, value) in values.iter().enumerate() {
                            if let Some(v) = value {
                                if let Some(ts) = timestamps.get(&id) {
                                    series.inner.insert(*ts, Value::Gauge(v));
                                }
                            }
                        }
                    }
                    DataType::List(field_type) => {
                        if field_type.data_type() == &DataType::UInt64 {
                            let list_array = column
                                .as_any()
                                .downcast_ref::<ListArray>()
                                .expect("Failed to downcast to ListArray");

                            let grouping_power = if let Some(Ok(v)) = grouping_power {
                                v
                            } else {
                                continue;
                            };

                            let max_value_power = if let Some(Ok(v)) = max_value_power {
                                v
                            } else {
                                continue;
                            };

                            for (id, value) in list_array.iter().enumerate() {
                                if let Some(list_value) = value {
                                    if let Some(ts) = timestamps.get(&id) {
                                        let data = list_value
                                            .as_any()
                                            .downcast_ref::<UInt64Array>()
                                            .expect("Failed to downcast to UInt64Array");

                                        let buckets: Vec<u64> = data.iter().flatten().collect();

                                        if let Ok(h) = Histogram::from_buckets(
                                            grouping_power,
                                            max_value_power,
                                            buckets,
                                        ) {
                                            series.inner.insert(*ts, Value::Histogram(h));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(data)
    }

    pub fn get(&self, name: &str, labels: &Labels) -> Option<TimeSeriesCollection> {
        if let Some(collection) = self.inner.get(name) {
            let collection = collection.filter(labels);

            if collection.inner.is_empty() {
                None
            } else {
                Some(collection)
            }
        } else {
            None
        }
    }

    pub fn sum(&self, metric: &str, labels: impl Into<Labels>) -> Option<TimeSeries> {
        self.get(metric, &labels.into())
            .map(|collection| collection.sum())
    }

    pub fn percentiles(&self, metric: &str, labels: impl Into<Labels>) -> Option<Vec<Vec<f64>>> {
        self.get(metric, &labels.into())
            .map(|collection| collection.percentiles())
    }

    pub fn cpu_avg(&self, metric: &str, labels: impl Into<Labels>) -> Option<TimeSeries> {
        if let Some(cores) = self.sum("cpu_cores", Labels::default()) {
            if let Some(collection) = self.get(metric, &labels.into()) {
                return Some(collection.sum().divide(&cores));
            }
        }

        None
    }

    pub fn cpu_heatmap(&self, metric: &str, labels: impl Into<Labels>) -> Option<Heatmap> {
        let mut heatmap = Heatmap::default();

        if let Some(collection) = self.get(metric, &labels.into()) {
            for (id, series) in collection.sum_by_cpu().drain(..).enumerate() {
                heatmap.inner.insert(id, series);
            }
        }

        if heatmap.inner.is_empty() {
            None
        } else {
            Some(heatmap)
        }
    }

    fn find_top_cgroups_avg(&self, metric: &str, n: usize) -> Vec<String> {
        let mut cgroups: HashMap<String, f64> = HashMap::new();

        // Collect all cgroups and their average usage
        if let Some(collection) = self.inner.get(metric) {
            let by_group = collection.sum_by_group();

            for (name, series) in by_group.iter() {
                let avg_usage = series.average();
                cgroups
                    .entry(name.clone())
                    .and_modify(|e| *e += avg_usage)
                    .or_insert(avg_usage);
            }
        }

        // Sort by usage and take top N
        let mut sorted: Vec<(String, f64)> = cgroups.into_iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(n);

        sorted.into_iter().map(|v| v.0).collect()
    }

    // New method to generate multi-series data for cgroups CPU usage
    pub fn top_cgroups_avg(&self, metric: &str, n: usize) -> Option<(Vec<Vec<f64>>, Vec<String>)> {
        // Get the top N cgroups first
        let cgroups = self.find_top_cgroups_avg(metric, n);
        if cgroups.is_empty() {
            return None;
        }

        // First, find the timestamps - get any series to extract timestamps
        let mut timestamps = Vec::new();

        if let Some(collection) = self.inner.get(metric) {
            if let Some(series) = collection.sum_by_group().values().next() {
                timestamps = series
                    .inner
                    .keys()
                    .map(|ts| *ts as f64 / 1000000000.0)
                    .collect();
            }
        }

        if timestamps.is_empty() {
            return None;
        }

        // Build a matrix of [timestamps, cgroup1_values, cgroup2_values, ...]
        let mut matrix = Vec::with_capacity(cgroups.len() + 1);
        matrix.push(timestamps.clone());

        // For each top cgroup, get its CPU usage timeseries
        for name in &cgroups {
            if let Some(collection) = self.inner.get(metric) {
                let by_group = collection.sum_by_group();

                if let Some(series) = by_group.get(name) {
                    let values = series.inner.values().copied().collect();
                    matrix.push(values);
                } else {
                    // If no data for this cgroup, add zeros
                    matrix.push(vec![0.0; timestamps.len()]);
                }
            }
        }

        Some((matrix, cgroups))
    }

    pub fn top_cgroups_avg_divide(
        &self,
        metric: &str,
        n: usize,
        divisor: f64,
    ) -> Option<(Vec<Vec<f64>>, Vec<String>)> {
        let (mut data, labels) = self.top_cgroups_avg(metric, n)?;

        for series in data.iter_mut().skip(1) {
            for v in series.iter_mut() {
                *v /= divisor;
            }
        }

        Some((data, labels))
    }
}

#[derive(Default)]
pub struct TimeSeriesCollection {
    inner: HashMap<Labels, InternalTimeSeries>,
}

impl TimeSeriesCollection {
    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    pub fn sum(&self) -> TimeSeries {
        let mut result = TimeSeries::default();

        for series in self.inner.values() {
            let mut prev_v = 0;
            let mut prev_ts = 0;

            for (time, value) in series.inner.iter() {
                match value {
                    Value::Counter(v) => {
                        if prev_ts != 0 {
                            let t_delta = (time - prev_ts) as f64 / 1000000000.0;
                            let v_delta = v.wrapping_sub(prev_v);

                            let rate = if v_delta < 1 << 63 {
                                v_delta as f64 / t_delta
                            } else {
                                0.0
                            };

                            if !result.inner.contains_key(time) {
                                result.inner.insert(*time, rate);
                            } else {
                                *result.inner.get_mut(time).unwrap() += rate;
                            }
                        }

                        prev_ts = *time;
                        prev_v = *v;
                    }
                    Value::Gauge(v) => {
                        if prev_ts != 0 {
                            if !result.inner.contains_key(time) {
                                result.inner.insert(*time, *v as f64);
                            } else {
                                *result.inner.get_mut(time).unwrap() += *v as f64;
                            }
                        }
                        prev_ts = *time;
                    }
                    Value::Histogram(_) => {}
                }
            }
        }

        result
    }

    pub fn percentiles(&self) -> Vec<Vec<f64>> {
        let mut tmp: BTreeMap<u64, Histogram> = BTreeMap::new();

        let mut result = vec![Vec::new(); PERCENTILES.len() + 1];

        // aggregate the histograms
        for series in self.inner.values() {
            for (time, value) in series.inner.iter() {
                if let Value::Histogram(h) = value {
                    tmp.entry(*time)
                        .and_modify(|sum| *sum = sum.wrapping_add(h).unwrap())
                        .or_insert(h.clone());
                }
            }
        }

        if tmp.is_empty() {
            println!("tmp is empty");
            return result;
        }

        let (_, mut prev) = tmp.pop_first().unwrap();

        for (time, curr) in tmp.iter() {
            let delta = curr.wrapping_sub(&prev).unwrap();

            result[0].push(*time as f64 / 1000000000.0);

            if let Ok(Some(percentiles)) = delta.percentiles(PERCENTILES) {
                for (id, (_, bucket)) in percentiles.iter().enumerate() {
                    result[id + 1].push(bucket.end() as f64);
                }
            } else {
                for series in result.iter_mut().skip(1) {
                    series.push(0.0);
                }
            }

            prev = curr.clone();
        }

        result
    }

    pub fn sum_by_cpu(&self) -> Vec<TimeSeries> {
        let mut result = Vec::new();

        let mut max_cpu = 0;

        for id in 0..1024 {
            let series = self
                .filter(&Labels {
                    inner: [("id".to_string(), format!("{id}"))].into(),
                })
                .sum();

            if !series.inner.is_empty() {
                max_cpu = id;
            }

            result.push(series);
        }

        result.truncate(max_cpu + 1);

        result
    }

    pub fn sum_by_group(&self) -> BTreeMap<String, TimeSeries> {
        let mut result = BTreeMap::new();
        let mut groups = BTreeSet::new();

        for labels in self.inner.keys() {
            if let Some(name) = labels.inner.get("name").cloned() {
                groups.insert(name);
            }
        }

        for group in groups {
            let series = self
                .filter(&Labels {
                    inner: [("name".to_string(), group.to_string())].into(),
                })
                .sum();

            result.insert(group, series);
        }

        result
    }
}

#[derive(Default, Eq, PartialEq, Hash, Clone, Debug)]
pub struct Labels {
    pub inner: BTreeMap<String, String>,
}

impl Labels {
    pub fn matches(&self, other: &Labels) -> bool {
        for (label, value) in other.inner.iter() {
            if let Some(v) = self.inner.get(label) {
                if v != value {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

impl From<&[(&str, &str)]> for Labels {
    fn from(other: &[(&str, &str)]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Labels {
    fn from(other: [(&str, &str); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(String, String); N]> for Labels {
    fn from(other: [(String, String); N]) -> Self {
        Labels {
            inner: other.iter().cloned().collect(),
        }
    }
}

impl<const N: usize> From<[(&str, String); N]> for Labels {
    fn from(other: [(&str, String); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
}

impl From<&mut dyn Iterator<Item = (&str, &str)>> for Labels {
    fn from(other: &mut dyn Iterator<Item = (&str, &str)>) -> Self {
        Self {
            inner: other.map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }
}

#[derive(Default, Clone)]
pub struct InternalTimeSeries {
    inner: BTreeMap<u64, Value>,
}

#[derive(Clone)]
pub enum Value {
    Counter(u64),
    Gauge(i64),
    Histogram(Histogram),
}

#[derive(Default, Clone)]
pub struct TimeSeries {
    inner: BTreeMap<u64, f64>,
}

impl TimeSeries {
    fn average(&self) -> f64 {
        if self.inner.is_empty() {
            return 0.0;
        }

        let mut sum = 0.0;
        let mut count = 0;

        for value in self.inner.values() {
            sum += *value;
            count += 1;
        }

        if count > 0 {
            sum / count as f64
        } else {
            0.0
        }
    }

    fn stddev(&self) -> f64 {
        if self.inner.is_empty() {
            return 0.0;
        }

        let values: Vec<f64> = self.inner.values().cloned().collect();

        if values.is_empty() {
            return 0.0;
        }

        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance =
            values.iter().map(|x| (*x - mean).powi(2)).sum::<f64>() / values.len() as f64;

        variance.sqrt()
    }

    fn divide_scalar(mut self, divisor: f64) -> Self {
        for value in self.inner.values_mut() {
            *value /= divisor;
        }

        self
    }

    fn divide(mut self, other: &TimeSeries) -> Self {
        // remove any times in this series that aren't in other
        let times: Vec<u64> = self.inner.keys().copied().collect();
        for time in times {
            if !other.inner.contains_key(&time) {
                let _ = self.inner.remove(&time);
            }
        }

        // divide all values with matching timestamps, leave nulls
        for (time, divisor) in other.inner.iter() {
            if let Some(v) = self.inner.get_mut(time) {
                *v /= divisor;
            }
        }

        self
    }

    fn multiply_scalar(mut self, multiplier: f64) -> Self {
        for value in self.inner.values_mut() {
            *value *= multiplier;
        }

        self
    }

    fn multiply(mut self, other: &TimeSeries) -> Self {
        // remove any times in this series that aren't in other
        let times: Vec<u64> = self.inner.keys().copied().collect();
        for time in times {
            if !other.inner.contains_key(&time) {
                let _ = self.inner.remove(&time);
            }
        }

        // multiply all values with matching timestamps, leave nulls
        for (time, multiplier) in other.inner.iter() {
            if let Some(v) = self.inner.get_mut(time) {
                *v *= multiplier;
            }
        }

        self
    }

    pub fn as_data(&self) -> Vec<Vec<f64>> {
        let mut times = Vec::new();
        let mut values = Vec::new();

        for (time, value) in self.inner.iter() {
            // convert time to unix epoch float seconds
            times.push(*time as f64 / 1000000000.0);
            values.push(*value);
        }

        vec![times, values]
    }
}

impl Add<TimeSeries> for TimeSeries {
    type Output = TimeSeries;

    fn add(self, other: TimeSeries) -> Self::Output {
        self.add(&other)
    }
}

impl Add<&TimeSeries> for TimeSeries {
    type Output = TimeSeries;

    fn add(mut self, other: &TimeSeries) -> Self::Output {
        // Add values from other TimeSeries where timestamps match
        for (time, value) in other.inner.iter() {
            if let Some(existing) = self.inner.get_mut(time) {
                *existing += value;
            } else {
                // If timestamp doesn't exist in self, add it
                self.inner.insert(*time, *value);
            }
        }

        self
    }
}

impl Div<TimeSeries> for TimeSeries {
    type Output = TimeSeries;
    fn div(self, other: TimeSeries) -> <Self as Div<TimeSeries>>::Output {
        self.divide(&other)
    }
}

impl Div<&TimeSeries> for TimeSeries {
    type Output = TimeSeries;
    fn div(self, other: &TimeSeries) -> <Self as Div<TimeSeries>>::Output {
        self.divide(other)
    }
}

impl Div<f64> for TimeSeries {
    type Output = TimeSeries;
    fn div(self, other: f64) -> <Self as Div<TimeSeries>>::Output {
        self.divide_scalar(other)
    }
}

impl Mul<TimeSeries> for TimeSeries {
    type Output = TimeSeries;
    fn mul(self, other: TimeSeries) -> <Self as Mul<TimeSeries>>::Output {
        self.multiply(&other)
    }
}

impl Mul<&TimeSeries> for TimeSeries {
    type Output = TimeSeries;
    fn mul(self, other: &TimeSeries) -> <Self as Mul<TimeSeries>>::Output {
        self.multiply(other)
    }
}

impl Mul<f64> for TimeSeries {
    type Output = TimeSeries;
    fn mul(self, other: f64) -> <Self as Mul<TimeSeries>>::Output {
        self.multiply_scalar(other)
    }
}

#[derive(Default, Clone)]
pub struct Heatmap {
    inner: BTreeMap<usize, TimeSeries>,
}

#[derive(Serialize)]
pub struct HeatmapData {
    pub time: Vec<f64>,
    pub data: Vec<Vec<f64>>,
    pub min_value: f64,
    pub max_value: f64,
}

impl Heatmap {
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
    fn div(self, other: Heatmap) -> <Self as Div<TimeSeries>>::Output {
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
    fn mul(self, other: Heatmap) -> <Self as Div<TimeSeries>>::Output {
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

impl Div<TimeSeries> for Heatmap {
    type Output = Heatmap;
    fn div(self, other: TimeSeries) -> <Self as Div<TimeSeries>>::Output {
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
    fn div(self, other: f64) -> <Self as Div<TimeSeries>>::Output {
        let mut result = Heatmap::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other);
        }

        result
    }
}
