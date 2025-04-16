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

mod collection;
mod heatmap;
mod labels;
mod timeseries;

pub use collection::Collection;
pub use heatmap::Heatmap;
pub use labels::Labels;
pub use timeseries::Timeseries;

#[derive(Default, Clone)]
pub struct RawTimeseries {
    inner: BTreeMap<u64, Value>,
}

#[derive(Clone)]
pub enum Value {
    Counter(u64),
    Gauge(i64),
    Histogram(Histogram),
}

#[derive(Default)]
pub struct Tsdb {
    inner: HashMap<String, Collection>,
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
                let series = collection.entry(labels).or_default();

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

    pub fn get(&self, name: &str, labels: &Labels) -> Option<Collection> {
        if let Some(collection) = self.inner.get(name) {
            let collection = collection.filter(labels);

            if collection.is_empty() {
                None
            } else {
                Some(collection)
            }
        } else {
            None
        }
    }

    pub fn sum(&self, metric: &str, labels: impl Into<Labels>) -> Option<Timeseries> {
        self.get(metric, &labels.into())
            .map(|collection| collection.sum())
    }

    pub fn percentiles(&self, metric: &str, labels: impl Into<Labels>) -> Option<Vec<Vec<f64>>> {
        self.get(metric, &labels.into())
            .map(|collection| collection.percentiles())
    }

    pub fn cpu_avg(&self, metric: &str, labels: impl Into<Labels>) -> Option<Timeseries> {
        if let Some(cores) = self.sum("cpu_cores", Labels::default()) {
            if let Some(collection) = self.get(metric, &labels.into()) {
                return Some(collection.sum() / cores);
            }
        }

        None
    }

    pub fn cpu_heatmap(&self, metric: &str, labels: impl Into<Labels>) -> Option<Heatmap> {
        let mut heatmap = Heatmap::default();

        if let Some(collection) = self.get(metric, &labels.into()) {
            for (id, series) in collection.sum_by_cpu().drain(..).enumerate() {
                heatmap.insert(id, series);
            }
        }

        if heatmap.is_empty() {
            None
        } else {
            Some(heatmap)
        }
    }

    pub fn cgroup(&self, metric: &str, labels: impl Into<Labels>) -> Option<CgroupCollection> {
        self.get(metric, &labels.into()).map(|v| v.by_group())
    }
}

#[derive(Default)]
pub struct CgroupCollection {
    inner: HashMap<String, Collection>,
}

impl CgroupCollection {
    pub fn insert(&mut self, name: String, collection: Collection) {
        self.inner.insert(name, collection);
    }

    pub fn sum(&self) -> CgroupTimeseries {
        let mut result = CgroupTimeseries::default();

        for (name, collection) in self.inner.iter() {
            result.inner.insert(name.to_string(), collection.sum());
        }

        result
    }
}

#[derive(Default, Clone)]
pub struct CgroupTimeseries {
    inner: HashMap<String, Timeseries>,
}

impl CgroupTimeseries {
    pub fn top_n(&self, n: usize, rank: fn(&Timeseries) -> f64) -> Vec<(String, Timeseries)> {
        let mut scores = Vec::new();

        for (name, series) in self.inner.iter() {
            let score = rank(series);

            scores.push((name, score));
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(n);

        let mut result = Vec::new();

        for (name, _) in scores.drain(..) {
            result.push((name.clone(), self.inner.get(name).unwrap().clone()));
        }

        result
    }

    pub fn worst_n(&self, n: usize, rank: fn(&Timeseries) -> f64) -> Vec<(String, Timeseries)> {
        let mut scores = Vec::new();

        for (name, series) in self.inner.iter() {
            let score = rank(series);

            scores.push((name, score));
        }

        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.reverse();
        scores.truncate(n);

        let mut result = Vec::new();

        for (name, _) in scores.drain(..) {
            result.push((name.clone(), self.inner.get(name).unwrap().clone()));
        }

        result
    }
}

impl Div<CgroupTimeseries> for CgroupTimeseries {
    type Output = CgroupTimeseries;
    fn div(self, other: CgroupTimeseries) -> <Self as Div<CgroupTimeseries>>::Output {
        let mut result = CgroupTimeseries::default();

        let mut this = self.inner.clone();

        for (name, series) in this.drain() {
            if let Some(other) = other.inner.get(&name) {
                result.inner.insert(name, series / other);
            }
        }

        result
    }
}

impl Div<Timeseries> for CgroupTimeseries {
    type Output = CgroupTimeseries;
    fn div(self, other: Timeseries) -> <Self as Div<Timeseries>>::Output {
        let mut result = CgroupTimeseries::default();

        let mut this = self.inner.clone();

        for (name, series) in this.drain() {
            result.inner.insert(name, series / other.clone());
        }

        result
    }
}

impl Div<f64> for CgroupTimeseries {
    type Output = CgroupTimeseries;
    fn div(self, other: f64) -> <Self as Div<Timeseries>>::Output {
        let mut result = CgroupTimeseries::default();

        let mut this = self.inner.clone();

        for (name, series) in this.drain() {
            result.inner.insert(name, series / other);
        }

        result
    }
}

pub fn average(timeseries: &Timeseries) -> f64 {
    timeseries.average()
}