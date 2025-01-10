use crate::exposition::http::CounterGroup;
use crate::exposition::http::GaugeGroup;
use metriken::RwLockHistogram;
use metriken::Value;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::time::SystemTime;

// These definitions are taken from `metriken-exposition` for compatibility with
// existing pipeline.

// TODO(bmartin): this representation can be optimized to be aware of counter
// and gauge groups

// TODO(bmartin): we can also consider splitting the metadata out into a
// separate endpoint and moving group metadata publication OOB.

#[derive(Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Counter {
    pub name: String,
    pub value: u64,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Gauge {
    pub name: String,
    pub value: i64,
    pub metadata: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Histogram {
    pub name: String,
    pub value: histogram::Histogram,
    pub metadata: HashMap<String, String>,
}

/// Contains a snapshot of metric readings.
#[derive(Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Snapshot {
    pub systemtime: SystemTime,

    #[serde(default)]
    pub metadata: HashMap<String, String>,

    pub counters: Vec<Counter>,
    pub gauges: Vec<Gauge>,
    pub histograms: Vec<Histogram>,
}

impl Snapshot {
    pub fn new() -> Self {
        let mut s = Self {
            systemtime: SystemTime::now(),
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
                                metadata.insert("group_id".to_string(), metric_id.to_string());

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
                                metadata.insert("group_id".to_string(), metric_id.to_string());

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

        s
    }
}
