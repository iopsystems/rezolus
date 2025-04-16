use super::*;

#[derive(Default)]
pub struct UntypedCollection {
    inner: HashMap<Labels, UntypedSeries>,
}

impl UntypedCollection {
    pub fn insert(&mut self, labels: Labels, series: UntypedSeries) {
        self.inner.insert(labels, series);
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