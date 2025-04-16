use std::collections::hash_map::Entry;
use super::*;

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

    // pub fn irate(&self) -> Gauges {

    // }

    pub fn sum(&self) -> Timeseries {
        let mut result = Timeseries::default();

        for series in self.inner.values() {
            let mut prev_v = 0;
            let mut prev_ts = 0;

            for (time, value) in series.iter() {
                if prev_ts != 0 {
                    let t_delta = (time - prev_ts) as f64 / 1000000000.0;
                    let v_delta = value.wrapping_sub(prev_v);

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
                prev_v = *value;
            }
        }

        result
    }

    pub fn by_cpu(&self) -> Vec<Timeseries> {
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

    pub fn by_group(&self) -> CgroupCounters {
        let mut result = CgroupCounters::default();
        let mut groups = BTreeSet::new();

        for labels in self.inner.keys() {
            if let Some(name) = labels.inner.get("name").cloned() {
                groups.insert(name);
            }
        }

        for group in groups {
            let collection = self
                .filter(&Labels {
                    inner: [("name".to_string(), group.to_string())].into(),
                });

            result.insert(group, collection);
        }

        result
    }
}

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

    // pub fn irate(&self) -> Gauges {

    // }

    pub fn sum(&self) -> Timeseries {
        let mut result = Timeseries::default();

        for series in self.inner.values() {
            let mut prev_ts = 0;

            for (time, value) in series.iter() {
                if prev_ts != 0 {
                    if !result.inner.contains_key(time) {
                        result.inner.insert(*time, *value as f64);
                    } else {
                        *result.inner.get_mut(time).unwrap() += *value as f64;
                    }
                }
                prev_ts = *time;
            }
        }

        result
    }

    pub fn by_cpu(&self) -> Vec<Timeseries> {
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

    pub fn by_group(&self) -> CgroupGauges {
        let mut result = CgroupGauges::default();
        let mut groups = BTreeSet::new();

        for labels in self.inner.keys() {
            if let Some(name) = labels.inner.get("name").cloned() {
                groups.insert(name);
            }
        }

        for group in groups {
            let collection = self
                .filter(&Labels {
                    inner: [("name".to_string(), group.to_string())].into(),
                });

            result.insert(group, collection);
        }

        result
    }
}



#[derive(Default)]
pub struct Collection {
    inner: HashMap<Labels, RawTimeseries>,
}

impl Collection {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn entry(&mut self, labels: Labels) -> Entry<'_, Labels, RawTimeseries> {
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
}