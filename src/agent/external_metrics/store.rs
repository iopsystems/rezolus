use super::types::{ExternalMetric, ExternalMetricValue, MetricKey};
use crate::warn;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Thread-safe store for external metrics with TTL-based expiration.
pub struct ExternalMetricsStore {
    metrics: RwLock<HashMap<MetricKey, ExternalMetric>>,
    reserved_names: HashSet<String>,
    ttl: Duration,
    max_metrics: usize,
    // Self-monitoring counters
    metrics_received: AtomicU64,
    parse_errors: AtomicU64,
    expired_count: AtomicU64,
    collisions_blocked: AtomicU64,
}

impl ExternalMetricsStore {
    pub fn new(ttl: Duration, max_metrics: usize, reserved_names: HashSet<String>) -> Self {
        Self {
            metrics: RwLock::new(HashMap::new()),
            reserved_names,
            ttl,
            max_metrics,
            metrics_received: AtomicU64::new(0),
            parse_errors: AtomicU64::new(0),
            expired_count: AtomicU64::new(0),
            collisions_blocked: AtomicU64::new(0),
        }
    }

    /// Insert or update a metric. Returns true if the metric was inserted/updated.
    /// Returns false if rejected due to collision, capacity, or other limits.
    pub fn upsert(
        &self,
        name: String,
        labels: HashMap<String, String>,
        value: ExternalMetricValue,
    ) -> bool {
        // Check for collision with internal metrics
        if self.reserved_names.contains(&name) {
            warn!(
                "external metric '{}' rejected: collides with internal metric",
                name
            );
            self.collisions_blocked.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let key = MetricKey::new(&name, &labels);
        let mut metrics = self.metrics.write();

        // Check capacity limit for new metrics
        if !metrics.contains_key(&key) && metrics.len() >= self.max_metrics {
            return false;
        }

        let metric = ExternalMetric {
            name,
            labels,
            value,
            last_updated: Instant::now(),
        };

        metrics.insert(key, metric);
        self.metrics_received.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Get all active (non-expired) metrics.
    pub fn get_active(&self) -> Vec<ExternalMetric> {
        let now = Instant::now();
        let metrics = self.metrics.read();

        metrics
            .values()
            .filter(|m| now.duration_since(m.last_updated) <= self.ttl)
            .cloned()
            .collect()
    }

    /// Remove expired metrics. Returns the number of metrics removed.
    pub fn cleanup(&self) -> usize {
        let now = Instant::now();
        let mut metrics = self.metrics.write();
        let before = metrics.len();

        metrics.retain(|_, m| now.duration_since(m.last_updated) <= self.ttl);

        let removed = before - metrics.len();
        if removed > 0 {
            self.expired_count
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    /// Get the current number of stored metrics.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.metrics.read().len()
    }

    /// Check if the store is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.metrics.read().is_empty()
    }

    /// Increment the parse error counter.
    pub fn record_parse_error(&self) {
        self.parse_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Get self-monitoring stats.
    #[allow(dead_code)]
    pub fn stats(&self) -> StoreStats {
        StoreStats {
            count: self.len(),
            received: self.metrics_received.load(Ordering::Relaxed),
            parse_errors: self.parse_errors.load(Ordering::Relaxed),
            expired: self.expired_count.load(Ordering::Relaxed),
            collisions_blocked: self.collisions_blocked.load(Ordering::Relaxed),
        }
    }
}

/// Self-monitoring statistics for the external metrics store.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StoreStats {
    pub count: usize,
    pub received: u64,
    pub parse_errors: u64,
    pub expired: u64,
    pub collisions_blocked: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(max_metrics: usize) -> ExternalMetricsStore {
        ExternalMetricsStore::new(Duration::from_secs(60), max_metrics, HashSet::new())
    }

    #[test]
    fn test_upsert_and_get() {
        let store = make_store(1000);

        let labels: HashMap<String, String> = [("env".to_string(), "test".to_string())]
            .into_iter()
            .collect();

        assert!(store.upsert(
            "test_counter".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(42),
        ));

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "test_counter");

        if let ExternalMetricValue::Counter(v) = active[0].value {
            assert_eq!(v, 42);
        } else {
            panic!("Expected counter value");
        }
    }

    #[test]
    fn test_max_metrics_limit() {
        let store = make_store(2);

        let labels = HashMap::new();

        assert!(store.upsert(
            "m1".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1)
        ));
        assert!(store.upsert(
            "m2".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(2)
        ));
        // Third metric should be rejected
        assert!(!store.upsert(
            "m3".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(3)
        ));

        assert_eq!(store.len(), 2);
    }

    #[test]
    fn test_update_existing() {
        let store = make_store(2);
        let labels = HashMap::new();

        assert!(store.upsert(
            "m1".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1)
        ));
        assert!(store.upsert(
            "m1".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(100)
        ));

        assert_eq!(store.len(), 1);
        let active = store.get_active();
        if let ExternalMetricValue::Counter(v) = active[0].value {
            assert_eq!(v, 100);
        } else {
            panic!("Expected counter value");
        }
    }

    #[test]
    fn test_cleanup_expired() {
        let store = ExternalMetricsStore::new(Duration::from_millis(10), 1000, HashSet::new());
        let labels = HashMap::new();

        store.upsert(
            "m1".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1),
        );

        // Wait for TTL to expire
        std::thread::sleep(Duration::from_millis(20));

        let removed = store.cleanup();
        assert_eq!(removed, 1);
        assert!(store.is_empty());
    }

    #[test]
    fn test_stats() {
        let store = make_store(1000);
        let labels = HashMap::new();

        store.upsert(
            "m1".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1),
        );
        store.upsert(
            "m2".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(2),
        );
        store.record_parse_error();

        let stats = store.stats();
        assert_eq!(stats.count, 2);
        assert_eq!(stats.received, 2);
        assert_eq!(stats.parse_errors, 1);
        assert_eq!(stats.expired, 0);
    }

    #[test]
    fn test_collision_with_reserved_name() {
        let reserved: HashSet<String> = ["cpu_usage".to_string()].into_iter().collect();
        let store = ExternalMetricsStore::new(Duration::from_secs(60), 1000, reserved);
        let labels = HashMap::new();

        // This should be rejected
        assert!(!store.upsert(
            "cpu_usage".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1)
        ));

        // This should succeed
        assert!(store.upsert(
            "my_custom_metric".to_string(),
            labels.clone(),
            ExternalMetricValue::Counter(1)
        ));

        assert_eq!(store.len(), 1);
        assert_eq!(store.stats().collisions_blocked, 1);
    }
}
