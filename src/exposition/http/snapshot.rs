use crate::exposition::http::CounterGroup;
use crate::exposition::http::GaugeGroup;
use metriken::RwLockHistogram;
use metriken::Value;
use metriken_exposition::{Counter, Gauge, Histogram, Snapshot, SnapshotV2};
use std::collections::HashMap;
use std::time::Duration;
use std::time::SystemTime;

pub fn create(timestamp: SystemTime, duration: Duration) -> Snapshot {
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
        let value = metric.value();

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
            }),
            Some(Value::Gauge(value)) => s.gauges.push(Gauge {
                name,
                value,
                metadata,
            }),
            Some(Value::Other(any)) => {
                if let Some(histogram) = any.downcast_ref::<RwLockHistogram>() {
                    if let Some(value) = histogram.load() {
                        metadata.insert(
                            "grouping_power".to_string(),
                            histogram.config().grouping_power().to_string(),
                        );
                        metadata.insert(
                            "max_value_power".to_string(),
                            histogram.config().max_value_power().to_string(),
                        );

                        s.histograms.push(Histogram {
                            name,
                            value,
                            metadata,
                        })
                    }
                } else if let Some(counters) = any.downcast_ref::<CounterGroup>() {
                    if let Some(c) = counters.load() {
                        for (counter_id, counter) in c.iter().enumerate() {
                            if *counter == 0 {
                                continue;
                            }
                            let mut metadata = metadata.clone();

                            metadata.insert("id".to_string(), counter_id.to_string());

                            if let Some(m) = counters.load_metadata(counter_id) {
                                for (k, v) in m {
                                    metadata.insert(k, v);
                                }
                            }

                            s.counters.push(Counter {
                                name: format!("{metric_id}x{counter_id}"),
                                value: *counter,
                                metadata,
                            })
                        }
                    }
                } else if let Some(gauges) = any.downcast_ref::<GaugeGroup>() {
                    if let Some(g) = gauges.load() {
                        for (gauge_id, gauge) in g.iter().enumerate() {
                            if *gauge == i64::MIN {
                                continue;
                            }

                            let mut metadata = metadata.clone();

                            metadata.insert("id".to_string(), gauge_id.to_string());

                            if let Some(m) = gauges.load_metadata(gauge_id) {
                                for (k, v) in m {
                                    metadata.insert(k, v);
                                }
                            }

                            s.gauges.push(Gauge {
                                name: format!("{metric_id}x{gauge_id}"),
                                value: *gauge,
                                metadata,
                            })
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Snapshot::V2(s)
}
