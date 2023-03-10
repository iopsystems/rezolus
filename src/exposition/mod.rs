// Copyright 2019 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::*;

mod http;
#[cfg(feature = "push_kafka")]
mod kafka;

pub use self::http::Http;
#[cfg(feature = "push_kafka")]
pub use self::kafka::KafkaProducer;

pub struct MetricsSnapshot {
    metrics: Arc<Metrics>,
    snapshot: HashMap<Metric, u64>,
    refreshed: Instant,
    count_label: Option<String>,
}

impl<'a> MetricsSnapshot {
    pub fn new(metrics: Arc<Metrics>, count_label: Option<&str>) -> Self {
        Self {
            metrics,
            snapshot: HashMap::new(),
            refreshed: Instant::now(),
            count_label: count_label.map(std::string::ToString::to_string),
        }
    }

    pub fn refresh(&mut self) {
        self.snapshot = self.metrics.snapshot();
        self.refreshed = Instant::now();
    }

    pub fn prometheus(&self) -> String {
        let mut data = Vec::new();
        for (metric, value) in &self.snapshot {
            let label = metric.statistic().name();
            let output = metric.output();
            match output {
                Output::Reading => {
                    data.push(format!("# TYPE {} gauge\n{} {}", label, label, value));
                }
                Output::Percentile(percentile) => {
                    data.push(format!(
                        "# TYPE {} gauge\n{}{{percentile=\"{:02}\"}} {}",
                        label, label, percentile, value
                    ));
                }
            }
        }
        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        let parts: Vec<&str> = content.split('/').collect();
        parts.join("_")
    }

    pub fn human(&self) -> String {
        let mut data = Vec::new();
        for (metric, value) in &self.snapshot {
            let label = metric.statistic().name();
            let output = metric.output();
            match output {
                Output::Reading => {
                    if let Some(ref count_label) = self.count_label {
                        data.push(format!("{}/{}: {}", label, count_label, value));
                    } else {
                        data.push(format!("{}: {}", label, value));
                    }
                }
                Output::Percentile(percentile) => {
                    data.push(format!("{}/histogram/p{:02}: {}", label, percentile, value));
                }
            }
        }
        data.sort();
        let mut content = data.join("\n");
        content += "\n";
        content
    }

    fn json(&self, pretty: bool) -> String {
        let mut head = "{".to_owned();
        if pretty {
            head += "\n  ";
        }
        let mut data = Vec::new();
        for (metric, value) in &self.snapshot {
            let label = metric.statistic().name();
            let output = metric.output();
            match output {
                Output::Reading => {
                    if let Some(ref count_label) = self.count_label {
                        data.push(format!("\"{}/{}\": {}", label, count_label, value));
                    } else {
                        data.push(format!("\"{}\": {}", label, value));
                    }
                }
                Output::Percentile(percentile) => {
                    data.push(format!(
                        "\"{}/histogram/p{:02}\": {}",
                        label, percentile, value
                    ));
                }
            }
        }
        data.sort();
        let body = if pretty {
            data.join(",\n  ")
        } else {
            data.join(",")
        };
        let mut content = head;
        content += &body;
        if pretty {
            content += "\n";
        }
        content += "}";
        content
    }
}
