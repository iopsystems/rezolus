use arrow::array::Int64Array;
use arrow::array::ListArray;
use arrow::array::UInt64Array;
use arrow::datatypes::DataType;
use histogram::Histogram;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;
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
mod series;

pub use collection::*;
pub use heatmap::Heatmap;
pub use labels::Labels;
pub use series::*;

#[derive(Default)]
pub struct Tsdb {
    sampling_interval_ms: u64,
    source: String,
    version: String,
    filename: String,
    counters: HashMap<String, CounterCollection>,
    gauges: HashMap<String, GaugeCollection>,
    histograms: HashMap<String, HistogramCollection>,
}

impl Tsdb {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut data = Tsdb::default();

        let file = File::open(path)?;
        let reader = SerializedFileReader::new(file).unwrap();
        let parquet_metadata = reader.metadata();
        let key_value_metadata = parquet_metadata
            .file_metadata()
            .key_value_metadata()
            .unwrap();

        let mut metadata = HashMap::new();

        for kv in key_value_metadata {
            metadata.insert(kv.key.clone(), kv.value.clone().unwrap_or("".to_string()));
        }

        let interval = metadata
            .get("sampling_interval_ms")
            .map(|v| v.parse::<u64>().expect("bad interval"))
            .unwrap_or(1000);
        data.sampling_interval_ms = interval;

        data.source = match metadata.get("source").map(|v| v.as_str()) {
            Some("rezolus") => "Rezolus".to_string(),
            _ => "unknown".to_string(),
        };

        data.version = match metadata.get("version").map(|v| v.as_str()) {
            Some(s) => s.to_string(),
            _ => "unknown".to_string(),
        };

        data.filename = path
            .file_name()
            .map(|v| v.to_str().unwrap_or("unknown"))
            .unwrap_or("unknown")
            .to_string();

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

    pub fn counters(&self, name: &str, labels: impl Into<Labels>) -> Option<CounterCollection> {
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

    pub fn gauges(&self, name: &str, labels: impl Into<Labels>) -> Option<GaugeCollection> {
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

    pub fn histograms(&self, name: &str, labels: impl Into<Labels>) -> Option<HistogramCollection> {
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

    #[allow(dead_code)]
    pub fn percentiles(
        &self,
        metric: &str,
        labels: impl Into<Labels>,
        percentiles: &[f64],
    ) -> Option<Vec<UntypedSeries>> {
        if let Some(collection) = self.histograms(metric, labels) {
            collection.sum().percentiles(percentiles)
        } else {
            None
        }
    }

    // sampling interval in seconds
    pub fn interval(&self) -> f64 {
        self.sampling_interval_ms as f64 / 1000.0
    }

    // data source
    pub fn source(&self) -> &str {
        &self.source
    }

    // data source version
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    // Get all counter metric names
    pub fn counter_names(&self) -> Vec<&str> {
        self.counters.keys().map(|s| s.as_str()).collect()
    }

    // Get all gauge metric names
    pub fn gauge_names(&self) -> Vec<&str> {
        self.gauges.keys().map(|s| s.as_str()).collect()
    }

    // Get all histogram metric names
    pub fn histogram_names(&self) -> Vec<&str> {
        self.histograms.keys().map(|s| s.as_str()).collect()
    }

    // Get labels for a specific counter metric
    pub fn counter_labels(&self, name: &str) -> Option<Vec<Labels>> {
        self.counters.get(name).map(|collection| {
            collection
                .iter()
                .map(|(labels, _)| labels.clone())
                .collect()
        })
    }

    // Get labels for a specific gauge metric
    pub fn gauge_labels(&self, name: &str) -> Option<Vec<Labels>> {
        self.gauges.get(name).map(|collection| {
            collection
                .iter()
                .map(|(labels, _)| labels.clone())
                .collect()
        })
    }

    // Get labels for a specific histogram metric
    pub fn histogram_labels(&self, name: &str) -> Option<Vec<Labels>> {
        self.histograms.get(name).map(|collection| {
            collection
                .iter()
                .map(|(labels, _)| labels.clone())
                .collect()
        })
    }
}
