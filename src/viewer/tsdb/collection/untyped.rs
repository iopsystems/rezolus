use super::*;

#[derive(Default)]
pub struct UntypedCollection {
    inner: HashMap<Labels, UntypedSeries>,
}

impl UntypedCollection {
    pub fn insert(&mut self, labels: Labels, series: UntypedSeries) {
        self.inner.insert(labels, series);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Labels, &UntypedSeries)> {
        self.inner.iter()
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
}
