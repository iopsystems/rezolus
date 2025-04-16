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

pub use collection::{Counters, Gauges, Histograms};
pub use heatmap::Heatmap;
pub use labels::Labels;
pub use timeseries::Timeseries;

#[derive(Default, Clone)]
pub struct RawTimeseries {
    inner: BTreeMap<u64, Value>,
}

#[derive(Clone)]
pub enum Value {
    Histogram(Histogram),
}

#[derive(Default)]
pub struct Tsdb {
    counters: HashMap<String, Counters>,
    gauges: HashMap<String, Gauges>,
    histograms: HashMap<String, Histograms>,
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

                let column = batch.column(id);

                match column.data_type() {
                    DataType::UInt64 => {
                        let counters = data.counters.entry(name.to_string()).or_default();
                        let series = counters.entry(labels).or_default();

                        let values = column
                            .as_any()
                            .downcast_ref::<UInt64Array>()
                            .expect("Failed to downcast");

                        for (id, value) in values.iter().enumerate() {
                            if let Some(v) = value {
                                if let Some(ts) = timestamps.get(&id) {
                                    series.insert(*ts, v);
                                }
                            }
                        }
                    }
                    DataType::Int64 => {
                        let collection = data.gauges.entry(name.to_string()).or_default();
                        let series = collection.entry(labels).or_default();

                        let values = column
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .expect("Failed to downcast");

                        for (id, value) in values.iter().enumerate() {
                            if let Some(v) = value {
                                if let Some(ts) = timestamps.get(&id) {
                                    series.insert(*ts, v);
                                }
                            }
                        }
                    }
                    DataType::List(field_type) => {
                        if field_type.data_type() == &DataType::UInt64 {
                            let collection = data.histograms.entry(name.to_string()).or_default();
                            let series = collection.entry(labels).or_default();

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
                                            series.insert(*ts, h);
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

    pub fn counters(&self, name: &str, labels: impl Into<Labels>) -> Option<Counters> {
        if let Some(counters) = self.counters.get(name) {
            let counters = counters.filter(&labels.into());

            if counters.is_empty() {
                None
            } else {
                Some(counters)
            }
        } else {
            None
        }
    }

    pub fn gauges(&self, name: &str, labels: impl Into<Labels>) -> Option<Gauges> {
        if let Some(gauges) = self.gauges.get(name) {
            let gauges = gauges.filter(&labels.into());

            if gauges.is_empty() {
                None
            } else {
                Some(gauges)
            }
        } else {
            None
        }
    }

    pub fn histograms(&self, name: &str, labels: impl Into<Labels>) -> Option<Histograms> {
        if let Some(histograms) = self.histograms.get(name) {
            let histograms = histograms.filter(&labels.into());

            if histograms.is_empty() {
                None
            } else {
                Some(histograms)
            }
        } else {
            None
        }
    }

    pub fn percentiles(&self, metric: &str, labels: impl Into<Labels>) -> Option<Vec<Vec<f64>>> {
        self.histograms(metric, labels)
            .map(|collection| collection.percentiles())
    }

    pub fn cpu_avg(&self, metric: &str, labels: impl Into<Labels>) -> Option<Timeseries> {
        if let Some(cores) = self.gauges("cpu_cores", ()).map(|v| v.sum()) {
            if let Some(collection) = self.counters(metric, labels) {
                return Some(collection.rate().sum() / cores);
            }
        }

        None
    }

    pub fn cpu_heatmap(&self, metric: &str, labels: impl Into<Labels>) -> Option<Heatmap> {
        let mut heatmap = Heatmap::default();

        if let Some(collection) = self.counters(metric, labels) {
            for (id, series) in collection.by_cpu().drain(..).enumerate() {
                heatmap.insert(id, series);
            }
        }

        if heatmap.is_empty() {
            None
        } else {
            Some(heatmap)
        }
    }
}

#[derive(Default)]
pub struct CgroupCounters {
    inner: HashMap<String, Counters>,
}

impl CgroupCounters {
    pub fn insert(&mut self, name: String, counters: Counters) {
        self.inner.insert(name, counters);
    }

    pub fn sum(&self) -> CgroupTimeseries {
        let mut result = CgroupTimeseries::default();

        for (name, counters) in self.inner.iter() {
            result.inner.insert(name.to_string(), counters.rate().sum());
        }

        result
    }
}

#[derive(Default)]
pub struct CgroupGauges {
    inner: HashMap<String, Gauges>,
}

impl CgroupGauges {
    pub fn insert(&mut self, name: String, gauges: Gauges) {
        self.inner.insert(name, gauges);
    }

    pub fn sum(&self) -> CgroupTimeseries {
        let mut result = CgroupTimeseries::default();

        for (name, gauges) in self.inner.iter() {
            result.inner.insert(name.to_string(), gauges.sum());
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

    pub fn bottom_n(&self, n: usize, rank: fn(&Timeseries) -> f64) -> Vec<(String, Timeseries)> {
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