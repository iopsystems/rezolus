use super::*;

/// Represents a collection of histogram timeseries keyed on label sets.
#[derive(Default)]
pub struct HistogramCollection {
    inner: HashMap<Labels, BTreeMap<u64, Histogram>>,
}

impl HistogramCollection {
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