use super::*;

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

impl From<&[(&str, &str)]> for Labels {
    fn from(other: &[(&str, &str)]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Labels {
    fn from(other: [(&str, &str); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl<const N: usize> From<[(String, String); N]> for Labels {
    fn from(other: [(String, String); N]) -> Self {
        Labels {
            inner: other.iter().cloned().collect(),
        }
    }
}

impl<const N: usize> From<[(&str, String); N]> for Labels {
    fn from(other: [(&str, String); N]) -> Self {
        Labels {
            inner: other
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }
}

impl From<&mut dyn Iterator<Item = (&str, &str)>> for Labels {
    fn from(other: &mut dyn Iterator<Item = (&str, &str)>) -> Self {
        Self {
            inner: other.map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }
}
