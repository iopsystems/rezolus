use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue, ExternalMetricsStore};
use crate::agent::*;

use metriken::Value;
use metriken_exposition::{Counter, Gauge, Histogram, Snapshot, SnapshotV2};

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

pub struct SnapshotBuilder {
    cached: Option<CachedSnapshot>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
    ttl: Duration,
    external_store: Option<Arc<ExternalMetricsStore>>,
}

struct CachedSnapshot {
    timestamp: Instant,
    snapshot: Snapshot,
}

impl SnapshotBuilder {
    pub fn new(
        config: Arc<Config>,
        samplers: Arc<Box<[Box<dyn Sampler>]>>,
        external_store: Option<Arc<ExternalMetricsStore>>,
    ) -> Self {
        Self {
            cached: None,
            samplers,
            ttl: config.general().ttl(),
            external_store,
        }
    }

    async fn refresh(&mut self) {
        let last = Instant::now();

        let timestamp = SystemTime::now();

        let s: Vec<_> = self
            .samplers
            .iter()
            .map(|s| s.refresh_with_logging())
            .collect();

        let start = Instant::now();
        futures::future::join_all(s).await;
        let duration = start.elapsed();
        debug!("sampling latency: {} us", duration.as_micros());

        let external_metrics = if let Some(store) = &self.external_store {
            store.cleanup();
            store.get_active()
        } else {
            Vec::new()
        };

        self.cached = Some(CachedSnapshot {
            snapshot: create(timestamp, duration, external_metrics),
            timestamp: last,
        });
    }

    pub async fn build(&mut self, now: Instant) -> &Snapshot {
        if self.cached.is_none()
            || now.duration_since(self.cached.as_ref().unwrap().timestamp) > self.ttl
        {
            self.refresh().await;
        }

        &self.cached.as_ref().unwrap().snapshot
    }
}

fn create(
    timestamp: SystemTime,
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
) -> Snapshot {
    let mut s = SnapshotV2 {
        systemtime: timestamp,
        duration,
        metadata: [
            ("source".to_string(), env!("CARGO_BIN_NAME").to_string()),
            ("version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ]
        .into(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let (value, stored_window) = metric.value_with_window();

        if value.is_none() {
            continue;
        }

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        let mut metadata: HashMap<String, String> =
            [("metric".to_string(), name.to_string())].into();

        for (k, v) in metric.metadata().iter() {
            metadata.insert(k.to_string(), v.to_string());
        }

        let name = format!("{metric_id}");

        match value {
            Some(Value::Counter(value)) => s.counters.push(Counter {
                name,
                value,
                metadata,
                window: stored_window,
            }),
            Some(Value::Gauge(value)) => s.gauges.push(Gauge {
                name,
                value,
                metadata,
                window: stored_window,
            }),
            Some(Value::CounterGroup(g)) => {
                for counter_id in 0..g.entries() {
                    // Atomic pair read: value + window under one lock, so a
                    // concurrent writer can never pair a fresh value with a
                    // stale window (drivehealth's async tear surface).
                    let (value, window) = g.load_with_window(counter_id);
                    let Some(value) = value else { continue };
                    if value == 0 {
                        continue;
                    }
                    let mut metadata = metadata.clone();

                    metadata.insert("id".to_string(), counter_id.to_string());

                    if let Some(m) = g.load_metadata(counter_id) {
                        for (k, v) in m {
                            metadata.insert(k, v);
                        }
                    }

                    s.counters.push(Counter {
                        name: format!("{metric_id}x{counter_id}"),
                        value,
                        metadata,
                        window,
                    })
                }
            }
            Some(Value::GaugeGroup(g)) => {
                for gauge_id in 0..g.entries() {
                    // Atomic pair read (see CounterGroup arm above).
                    let (value, window) = g.load_with_window(gauge_id);
                    let Some(value) = value else { continue };
                    if value == i64::MIN {
                        continue;
                    }

                    let mut metadata = metadata.clone();

                    metadata.insert("id".to_string(), gauge_id.to_string());

                    if let Some(m) = g.load_metadata(gauge_id) {
                        for (k, v) in m {
                            metadata.insert(k, v);
                        }
                    }

                    s.gauges.push(Gauge {
                        name: format!("{metric_id}x{gauge_id}"),
                        value,
                        metadata,
                        window,
                    })
                }
            }
            Some(Value::Histogram(h)) => {
                if let Some(value) = h.load() {
                    metadata.insert(
                        "grouping_power".to_string(),
                        h.config().grouping_power().to_string(),
                    );
                    metadata.insert(
                        "max_value_power".to_string(),
                        h.config().max_value_power().to_string(),
                    );

                    s.histograms.push(Histogram {
                        name,
                        value,
                        metadata,
                        window: stored_window,
                    })
                }
            }
            _ => {}
        }
    }

    for metric in external_metrics.into_iter() {
        // Capture the window before metric fields are consumed by the moves below.
        // Window is Copy so this is free; precedence level 2 (external source stamp).
        let window = metric.window;

        let mut metadata: HashMap<String, String> = [
            ("metric".to_string(), metric.name.clone()),
            ("source".to_string(), "external".to_string()),
        ]
        .into();

        for (k, v) in metric.labels {
            metadata.insert(k, v);
        }

        let name = String::new();

        match metric.value {
            ExternalMetricValue::Counter(value) => {
                s.counters.push(Counter {
                    name,
                    value,
                    metadata,
                    window,
                });
            }
            ExternalMetricValue::Gauge(value) => {
                s.gauges.push(Gauge {
                    name,
                    value,
                    metadata,
                    window,
                });
            }
            ExternalMetricValue::Histogram {
                grouping_power,
                max_value_power,
                buckets,
            } => {
                if let Ok(value) =
                    histogram::Histogram::from_buckets(grouping_power, max_value_power, buckets)
                {
                    metadata.insert("grouping_power".to_string(), grouping_power.to_string());
                    metadata.insert("max_value_power".to_string(), max_value_power.to_string());

                    s.histograms.push(Histogram {
                        name,
                        value,
                        metadata,
                        window,
                    });
                }
            }
        }
    }

    Snapshot::V2(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue};
    use metriken::Window;
    use std::time::{Duration, SystemTime};

    #[test]
    fn external_metric_carries_its_own_window_not_fleet_time() {
        let win = Window::new(1_000, 2_000);
        let ext = ExternalMetric {
            name: "ext_counter".into(),
            labels: Default::default(),
            value: ExternalMetricValue::Counter(7),
            last_updated: std::time::Instant::now(),
            window: Some(win),
        };
        let snap = create(SystemTime::now(), Duration::from_secs(5), vec![ext]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| c.metadata.get("metric").map(String::as_str) == Some("ext_counter"))
            .expect("external counter present");
        assert_eq!(c.window, Some(win), "external window preserved, not fleet");
    }
}
