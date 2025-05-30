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

    pub fn cpu_avg(&self, metric: &str, labels: impl Into<Labels>) -> Option<UntypedSeries> {
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
            for (id, series) in collection.rate().by_id().inner.iter() {
                heatmap.insert(*id, series.clone());
            }
        }

        if heatmap.is_empty() {
            None
        } else {
            Some(heatmap)
        }
    }

    // sampling interval in seconds
    pub fn interval(&self) -> f64 {
        self.sampling_interval_ms as f64 / 1000.0
    }

    // data source
    pub fn source(&self) -> String {
        self.source.clone()
    }

    // data source version
    pub fn version(&self) -> String {
        self.version.clone()
    }

    pub fn filename(&self) -> String {
        self.filename.clone()
    }
}

#[derive(Default, Clone)]
pub struct NamedSeries {
    inner: BTreeMap<String, UntypedSeries>,
}

impl NamedSeries {
    fn ranked_n(
        &self,
        n: usize,
        rank: fn(&UntypedSeries) -> f64,
        ascending: bool,
    ) -> Vec<(String, UntypedSeries)> {
        let mut scores = Vec::new();

        for (name, series) in self.inner.iter() {
            let score = rank(series);
            scores.push((name, (score * 1_000_000_000.0) as u64));
        }

        if ascending {
            scores.sort_by(|a, b| a.1.cmp(&b.1));
        } else {
            scores.sort_by(|a, b| b.1.cmp(&a.1));
        }

        if scores.len() > n {
            let mut result = Vec::new();
            let mut other_series = UntypedSeries::default();

            for (name, _score) in &scores[0..n] {
                result.push((
                    name.to_string(),
                    self.inner.get(name.as_str()).unwrap().clone(),
                ));
            }

            for (name, _score) in &scores[n..] {
                if let Some(series) = self.inner.get(name.as_str()) {
                    if other_series.inner.is_empty() {
                        other_series = series.clone();
                    } else {
                        for (time, value) in series.inner.iter() {
                            if other_series.inner.contains_key(time) {
                                *other_series.inner.get_mut(time).unwrap() += value;
                            } else {
                                other_series.inner.insert(*time, *value);
                            }
                        }
                    }
                }
            }

            if !other_series.inner.is_empty() {
                result.push(("Other".to_string(), other_series));
            }

            result
        } else {
            let mut result = Vec::new();

            for (name, _) in scores.drain(..) {
                result.push((name.clone(), self.inner.get(name.as_str()).unwrap().clone()));
            }

            result
        }
    }

    pub fn top_n(&self, n: usize, rank: fn(&UntypedSeries) -> f64) -> Vec<(String, UntypedSeries)> {
        self.ranked_n(n, rank, false)
    }

    pub fn bottom_n(
        &self,
        n: usize,
        rank: fn(&UntypedSeries) -> f64,
    ) -> Vec<(String, UntypedSeries)> {
        self.ranked_n(n, rank, true)
    }
}

impl Div<NamedSeries> for NamedSeries {
    type Output = NamedSeries;
    fn div(self, other: NamedSeries) -> <Self as Div<NamedSeries>>::Output {
        let mut result = NamedSeries::default();

        let mut this = self.inner.clone();

        while let Some((name, series)) = this.pop_first() {
            if let Some(other) = other.inner.get(&name) {
                result.inner.insert(name, series / other);
            }
        }

        result
    }
}

impl Div<UntypedSeries> for NamedSeries {
    type Output = NamedSeries;
    fn div(self, other: UntypedSeries) -> <Self as Div<UntypedSeries>>::Output {
        let mut result = NamedSeries::default();

        let mut this = self.inner.clone();

        while let Some((name, series)) = this.pop_first() {
            result.inner.insert(name, series / other.clone());
        }

        result
    }
}

impl Div<f64> for NamedSeries {
    type Output = NamedSeries;
    fn div(self, other: f64) -> <Self as Div<UntypedSeries>>::Output {
        let mut result = NamedSeries::default();

        let mut this = self.inner.clone();

        while let Some((name, series)) = this.pop_first() {
            result.inner.insert(name, series / other);
        }

        result
    }
}

#[derive(Default, Clone)]
pub struct IndexedSeries {
    inner: BTreeMap<usize, UntypedSeries>,
}

impl Div<IndexedSeries> for IndexedSeries {
    type Output = IndexedSeries;
    fn div(self, other: IndexedSeries) -> <Self as Div<IndexedSeries>>::Output {
        let mut result = IndexedSeries::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            if let Some(other) = other.inner.get(&id) {
                result.inner.insert(id, series / other);
            }
        }

        result
    }
}

impl Div<UntypedSeries> for IndexedSeries {
    type Output = IndexedSeries;
    fn div(self, other: UntypedSeries) -> <Self as Div<UntypedSeries>>::Output {
        let mut result = IndexedSeries::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other.clone());
        }

        result
    }
}

impl Div<f64> for IndexedSeries {
    type Output = IndexedSeries;
    fn div(self, other: f64) -> <Self as Div<UntypedSeries>>::Output {
        let mut result = IndexedSeries::default();

        let mut this = self.inner.clone();

        while let Some((id, series)) = this.pop_first() {
            result.inner.insert(id, series / other);
        }

        result
    }
}

pub fn average(series: &UntypedSeries) -> f64 {
    series.average()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_top_bottom_n() {
        let mut named = NamedSeries::default();

        let mut a = UntypedSeries::default();
        a.inner.insert(1, 1.0);
        a.inner.insert(2, 2.0); // avg 1.5

        let mut b = UntypedSeries::default();
        b.inner.insert(1, 2.0);
        b.inner.insert(2, 4.0); // avg 3.0

        let mut c = UntypedSeries::default();
        c.inner.insert(1, 0.5);
        c.inner.insert(2, 1.0); // avg 0.75

        named.inner.insert("a".into(), a);
        named.inner.insert("b".into(), b);
        named.inner.insert("c".into(), c.clone());

        let top = named.top_n(2, average);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].0, "b");
        assert_eq!(top[1].0, "a");
        assert_eq!(top[2].0, "Other");
        assert_eq!(top[2].1.inner.get(&1).copied().unwrap(), 0.5);
        assert_eq!(top[2].1.inner.get(&2).copied().unwrap(), 1.0);

        let bottom = named.bottom_n(2, average);
        assert_eq!(bottom.len(), 3);
        assert_eq!(bottom[0].0, "c");
        assert_eq!(bottom[1].0, "a");
        assert_eq!(bottom[2].0, "Other");
        assert_eq!(bottom[2].1.inner.get(&1).copied().unwrap(), 2.0);
        assert_eq!(bottom[2].1.inner.get(&2).copied().unwrap(), 4.0);
    }
}
