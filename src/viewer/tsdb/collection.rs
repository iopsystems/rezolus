use std::collections::hash_map::Entry;
use super::*;

/// Represents a collection of counter timeseries keyed on label sets.
#[derive(Default)]
pub struct Counters {
    inner: HashMap<Labels, BTreeMap<u64, u64>>,
}

impl Counters {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, BTreeMap<u64, u64>> {
        self.inner.entry(labels)
    }

    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    pub fn rate(&self) -> Untyped {
        let mut result = Untyped::default();

        for (labels, series) in self.inner.iter() {
            let mut rates = UntypedSeries::default();

            let mut prev: Option<(u64, u64)> = None;

            for (ts, value) in series.iter() {
                if let Some((prev_ts, prev_v)) = prev {
                    let delta = value.wrapping_sub(prev_v);

                    if delta < 1<<63 {
                        let duration = ts.wrapping_sub(prev_ts);

                        let rate = delta as f64 / (duration as f64 / 1000000000.0);

                        rates.inner.insert(*ts, rate);
                     }
                 }

                prev = Some((*ts, *value));
             }

            result.inner.insert(labels.clone(), rates);
         }

         result
    }
}

/// Represents a collection of gauge timeseries keyed on label sets.
#[derive(Default)]
pub struct Gauges {
    inner: HashMap<Labels, BTreeMap<u64, i64>>,
}

impl Gauges {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, BTreeMap<u64, i64>> {
        self.inner.entry(labels)
    }

    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    /// Convert the gauge collection to a untyped collection by converting `i64`
    /// gauges to `f64` representation.
    pub fn untyped(&self) -> Untyped {
        let mut result = Untyped::default();

        for (labels, series) in self.inner.iter() {
            let series = UntypedSeries { inner: series.iter().map(|(k, v)| (*k, *v as f64)).collect() };
            result.inner.insert(labels.clone(), series);
        }

        result
    }

    /// Convenience function to sum all gauges in the collection into a single
    /// `UntypedSeries`
    pub fn sum(&self) -> UntypedSeries {
        self.untyped().sum()
    }
}

/// Represents a collection of histogram timeseries keyed on label sets.
#[derive(Default)]
pub struct Histograms {
    inner: HashMap<Labels, BTreeMap<u64, Histogram>>,
}

impl Histograms {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, BTreeMap<u64, Histogram>> {
        self.inner.entry(labels)
    }

    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
    }

    pub fn percentiles(&self) -> Vec<Vec<f64>> {
        let mut tmp: BTreeMap<u64, Histogram> = BTreeMap::new();

        let mut result = vec![Vec::new(); PERCENTILES.len() + 1];

        // aggregate the histograms
        for series in self.inner.values() {
            for (time, value) in series.iter() {
                tmp.entry(*time)
                    .and_modify(|sum| *sum = sum.wrapping_add(value).unwrap())
                    .or_insert(value.clone());
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
}

#[derive(Default)]
pub struct Untyped {
    inner: HashMap<Labels, UntypedSeries>,
}

impl Untyped {
    pub fn filter(&self, labels: &Labels) -> Self {
        let mut result = Self::default();

        for (k, v) in self.inner.iter() {
            if k.matches(labels) {
                result.inner.insert(k.clone(), v.clone());
            }
        }

        result
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

    pub fn by_id(&self) -> IndexedSeries {
        let mut result = BTreeMap::new();
        let mut ids = BTreeSet::new();

        for labels in self.inner.keys() {
            if let Some(Ok(id)) = labels.inner.get("id").cloned().map(|v| v.parse::<usize>()) {
                ids.insert(id);
            }
        }

        for id in ids {
            let series = self
                .filter(&Labels {
                    inner: [("id".to_string(), id.to_string())].into(),
                })
                .sum();

            result.insert(id, series);
        }

        IndexedSeries { inner: result }
    }

    pub fn by_name(&self) -> NamedSeries {
        let mut result = BTreeMap::default();
        let mut names = BTreeSet::new();

        for labels in self.inner.keys() {
            if let Some(name) = labels.inner.get("name").cloned() {
                names.insert(name);
            }
        }

        for name in names {
            let series = self
                .filter(&Labels {
                    inner: [("name".to_string(), name.to_string())].into(),
                })
                .sum();

            result.insert(name, series);
        }

        NamedSeries { inner: result }
    }
}

