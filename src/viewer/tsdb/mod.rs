use arrow::array::Int64Array;
use arrow::array::UInt64Array;
use arrow::datatypes::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
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

                let meta = field.metadata();

                let name = if let Some(n) = meta.get("metric") {
                    n
                } else {
                    continue;
                };

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
                                    // for counters, we care about rates. So calculate the rate here.
                                    if series.init {
                                        let (prev_ts, prev_v) = series.prev;
                                        let rate = v.wrapping_sub(prev_v) as f64
                                            / ((*ts - prev_ts) as f64 / 1000000000.0);
                                        series.inner.insert(*ts, rate);
                                        series.prev = (*ts, v);
                                    } else {
                                        series.prev = (*ts, v);
                                        series.init = true;
                                    }
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
                                    if series.init {
                                        series.inner.insert(*ts, v as f64);
                                    } else {
                                        series.init = true;
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

    pub fn cpu_avg(&self, metric: &str, labels: impl Into<Labels>) -> Option<TimeSeries> {
        if let Some(cores) = self.sum("cpu_cores", Labels::default()) {
            if let Some(collection) = self.get(metric, &labels.into()) {
                let mut sum = collection.sum();
                sum.divide(&cores);
                return Some(sum);
            }
        }

        None
    }

    pub fn cpu_heatmap(&self, metric: &str, labels: impl Into<Labels>) -> Heatmap {
        let mut heatmap = Heatmap::default();

        // if let Some(collection) = self.get(metric, &labels.into()) {
        //     for series in collection.sum_by_cpu().iter() {

        //     }
        // }
        for series in self
            .get(metric, &labels.into())
            .unwrap()
            .sum_by_cpu()
            .drain(..)
        {
            let series = series.divide_scalar(1000000000.0);
            // let d = series.as_data();

            heatmap.inner.push(series.clone());

            // if heatmap.is_empty() {
            //     heatmap.push(d[0].clone());
            // }

            // heatmap.push(d[1].clone());
        }

        heatmap
    }
}

#[derive(Default)]
pub struct TimeSeriesCollection {
    inner: HashMap<Labels, TimeSeries>,
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
            for (time, value) in series.inner.iter() {
                if !result.inner.contains_key(time) {
                    result.inner.insert(*time, 0.0);
                }

                *result.inner.get_mut(time).unwrap() += value;
            }
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

// impl<T> From<T> for Labels
// where
//     T: Into<BTreeMap<String, String>>,
// {
//     fn from(other: T) -> Self {
//         Self {
//             inner: other.into(),
//         }
//     }
// }

impl<T> From<T> for Labels
where
    T: Into<BTreeMap<&'static str, &'static str>>,
{
    fn from(other: T) -> Self {
        let labels: BTreeMap<&'static str, &'static str> = other.into();

        let inner = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        Self { inner }
    }
}

#[derive(Default, Clone)]
pub struct TimeSeries {
    inner: BTreeMap<u64, f64>,
    prev: (u64, u64),
    init: bool,
}

impl TimeSeries {
    pub fn divide_scalar(mut self, divisor: f64) -> Self {
        for value in self.inner.values_mut() {
            *value /= divisor;
        }

        self
    }

    pub fn divide(&mut self, other: &TimeSeries) {
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
    }

    pub fn multiply_scalar(mut self, multiplier: f64) -> Self {
        for value in self.inner.values_mut() {
            *value *= multiplier;
        }

        self
    }

    pub fn multiply(&mut self, other: &TimeSeries) {
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

#[derive(Default, Clone)]
pub struct Heatmap {
    inner: Vec<TimeSeries>,
}
