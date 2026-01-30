use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

/// A metric received from an external source via Unix domain socket.
#[derive(Debug, Clone)]
pub struct ExternalMetric {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub value: ExternalMetricValue,
    pub last_updated: Instant,
}

/// Per-connection context for tracking session state and limits.
#[derive(Debug, Default)]
pub struct ConnectionContext {
    /// Session labels that are automatically applied to all metrics from this connection.
    pub session_labels: HashMap<String, String>,
    /// Number of unique metrics submitted by this connection.
    pub metric_count: usize,
}

/// The value type for an external metric.
#[derive(Debug, Clone)]
pub enum ExternalMetricValue {
    Counter(u64),
    Gauge(i64),
    Histogram {
        grouping_power: u8,
        max_value_power: u8,
        buckets: Vec<u64>,
    },
}

/// A key that uniquely identifies a metric by name and label set.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MetricKey {
    pub name: String,
    pub labels_hash: u64,
}

impl MetricKey {
    pub fn new(name: &str, labels: &HashMap<String, String>) -> Self {
        Self {
            name: name.to_string(),
            labels_hash: hash_labels(labels),
        }
    }
}

impl Hash for MetricKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.labels_hash.hash(state);
    }
}

/// Hash a label set deterministically by sorting keys.
fn hash_labels(labels: &HashMap<String, String>) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();

    // Sort keys for deterministic hashing
    let mut pairs: Vec<_> = labels.iter().collect();
    pairs.sort_by_key(|(k, _)| *k);

    for (k, v) in pairs {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_key_equality() {
        let labels1: HashMap<String, String> = [("env".to_string(), "prod".to_string())]
            .into_iter()
            .collect();
        let labels2: HashMap<String, String> = [("env".to_string(), "prod".to_string())]
            .into_iter()
            .collect();

        let key1 = MetricKey::new("test_metric", &labels1);
        let key2 = MetricKey::new("test_metric", &labels2);

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_metric_key_different_labels() {
        let labels1: HashMap<String, String> = [("env".to_string(), "prod".to_string())]
            .into_iter()
            .collect();
        let labels2: HashMap<String, String> = [("env".to_string(), "dev".to_string())]
            .into_iter()
            .collect();

        let key1 = MetricKey::new("test_metric", &labels1);
        let key2 = MetricKey::new("test_metric", &labels2);

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_hash_labels_order_independent() {
        let mut labels1 = HashMap::new();
        labels1.insert("a".to_string(), "1".to_string());
        labels1.insert("b".to_string(), "2".to_string());

        let mut labels2 = HashMap::new();
        labels2.insert("b".to_string(), "2".to_string());
        labels2.insert("a".to_string(), "1".to_string());

        assert_eq!(hash_labels(&labels1), hash_labels(&labels2));
    }
}
