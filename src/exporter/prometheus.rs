use super::*;

pub async fn prometheus() -> String {
    let snapshot = { SNAPSHOT.lock().clone() };

    let mut data = Vec::new();

    if snapshot.is_none() {
        return "".to_owned();
    }

    let mut snapshot = snapshot.unwrap();

    let timestamp = snapshot
        .systemtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    for metric in snapshot.counters.drain(..) {
        data.push(metric.format(timestamp));
    }

    for metric in snapshot.gauges.drain(..) {
        data.push(metric.format(timestamp));
    }

    for metric in snapshot.histograms.drain(..) {
        data.push(metric.format(timestamp));
    }

    data.sort();
    data.dedup();
    data.join("\n") + "\n"
}

trait SimplePrometheusMetric {
    fn name(&self) -> &str;
    fn kind(&self) -> &str;
    fn metadata(&self) -> String;
    fn value(&self) -> String;
}

trait PrometheusFormat {
    fn format(&self, timestamp: u128) -> String;
}

impl<T: SimplePrometheusMetric> PrometheusFormat for T {
    fn format(&self, timestamp: u128) -> String {
        let name = self.name();
        let metadata = self.metadata();

        let name_with_metadata = if metadata.is_empty() {
            name.to_string()
        } else {
            format!("{name}{{{metadata}}}")
        };

        let value = self.value();
        let kind = self.kind();

        format!("# TYPE {name} {kind}\n{name_with_metadata} {value} {timestamp}")
    }
}

impl SimplePrometheusMetric for Counter {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &str {
        "counter"
    }

    fn metadata(&self) -> String {
        format_metadata(&self.metadata)
    }

    fn value(&self) -> String {
        format!("{}", self.value)
    }
}

impl SimplePrometheusMetric for Gauge {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &str {
        "gauge"
    }

    fn metadata(&self) -> String {
        format_metadata(&self.metadata)
    }

    fn value(&self) -> String {
        format!("{}", self.value)
    }
}

impl PrometheusFormat for Histogram {
    fn format(&self, timestamp: u128) -> String {
        let name = &self.name;
        let metadata = format_metadata(&self.metadata);

        // we need to export a total count (free-running)
        let mut count = 0;
        // we also need to export a total sum of all observations
        // which is also free-running
        let mut sum = 0;

        let mut entry = format!("# TYPE {name}_distribution histogram\n");
        for bucket in &self.value {
            // add this bucket's sum of observations
            sum += bucket.count() * bucket.end();

            // add the count to the aggregate
            count += bucket.count();

            if metadata.is_empty() {
                entry += &format!(
                    "{name}_distribution_bucket{{le=\"{}\"}} {count} {timestamp}\n",
                    bucket.end()
                );
            } else {
                entry += &format!(
                    "{name}_distribution_bucket{{{metadata}, le=\"{}\"}} {count} {timestamp}\n",
                    bucket.end()
                );
            }
        }

        if metadata.is_empty() {
            entry += &format!("{name}_distribution_bucket{{le=\"+Inf\"}} {count} {timestamp}\n");
            entry += &format!("{name}_distribution_count {count} {timestamp}\n");
            entry += &format!("{name}_distribution_sum {sum} {timestamp}");
        } else {
            entry += &format!(
                "{name}_distribution_bucket{{{metadata}, le=\"+Inf\"}} {count} {timestamp}\n"
            );
            entry += &format!("{name}_distribution_count{{{metadata}}} {count} {timestamp}\n");
            entry += &format!("{name}_distribution_sum{{{metadata}}} {sum} {timestamp}");
        }

        entry
    }
}

/// The label set as the exposition format wants it: sorted, and without
/// internal labels. Prometheus reserves `__`-prefixed names and drops them
/// after relabeling, so a `__uid__` sent here would cost bytes on every scrape
/// and survive nowhere; it is the agent's series identity, not the scraper's.
fn format_metadata(metadata: &HashMap<String, String>) -> String {
    let mut metadata: Vec<String> = metadata
        .iter()
        .filter(|(key, _)| !metriken_query::is_internal_label(key))
        .map(|(key, value)| format!("{key}=\"{value}\""))
        .collect();
    metadata.sort();
    metadata.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_labels_are_not_exported() {
        let metadata: HashMap<String, String> = [
            ("__uid__", "0123456789abcdef"),
            ("comm", "redis"),
            ("id", "7"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(format_metadata(&metadata), "comm=\"redis\", id=\"7\"");
    }
}
