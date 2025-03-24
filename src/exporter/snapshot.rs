use super::*;

/// Produces a snapshot from a previous and current snapshot
pub fn snapshot(config: &Config, mut previous: Snapshot, mut current: Snapshot) -> SnapshotV2 {
    let mut snapshot = SnapshotV2 {
        systemtime: current.systemtime(),
        duration: current
            .systemtime()
            .duration_since(previous.systemtime())
            .unwrap(),
        metadata: current.metadata(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };

    for curr in current.counters() {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name.to_string()
        } else {
            continue;
        };

        snapshot.counters.push(Counter {
            name,
            value: curr.value,
            metadata,
        })
    }

    for curr in current.gauges() {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name.to_string()
        } else {
            continue;
        };

        snapshot.gauges.push(Gauge {
            name,
            value: curr.value,
            metadata,
        })
    }

    'outer: for (prev, curr) in previous.histograms().iter().zip(current.histograms()) {
        // optionally, generate summaries from histograms
        //
        // This requires some care as we are responsible for detecting if the
        // histogram has reset. This would happen if the agent has restarted. In
        // that case we skip summary exposition until the next snapshot.
        if config.prometheus().summaries() {
            let mut metadata = curr.metadata.clone();

            // the real metric name is encoded in the metadata
            let name = if let Some(name) = metadata.remove("metric") {
                name
            } else {
                continue;
            };

            // histograms have extra metadata we should remove
            let _ = metadata.remove("grouping_power");
            let _ = metadata.remove("max_value_power");

            // calculate the delta histogram
            let delta = if let Ok(delta) = curr.value.wrapping_sub(&prev.value) {
                delta
            } else {
                continue;
            };

            // detect reset by looking for buckets with unusually large deltas
            for count in delta.iter().map(|bucket| bucket.count()) {
                if count > 1 << 63 {
                    continue 'outer;
                }
            }

            if let Ok(Some(percentiles)) = delta.percentiles(&[50.0, 90.0, 99.0, 99.9, 99.99]) {
                for (percentile, value) in percentiles.into_iter().map(|(p, b)| (p, b.end())) {
                    if let Ok(value) = value.try_into() {
                        let mut metadata = metadata.clone();
                        metadata.insert("percentile".to_string(), percentile.to_string());

                        snapshot.gauges.push(Gauge {
                            name: name.clone(),
                            value,
                            metadata,
                        })
                    }
                }
            }
        }

        // optionally, export full histograms
        if config.prometheus().histograms() {
            let mut metadata = curr.metadata.clone();

            // the real metric name is encoded in the metadata
            let name = if let Some(name) = metadata.remove("metric") {
                name.to_string()
            } else {
                continue;
            };

            // downsample the histogram if required
            let value = if config.prometheus().histogram_grouping_power()
                >= curr.value.config().grouping_power()
            {
                curr.value.clone()
            } else {
                curr.value
                    .downsample(config.prometheus().histogram_grouping_power())
                    .unwrap()
            };

            snapshot.histograms.push(Histogram {
                name,
                value,
                metadata,
            })
        }
    }

    snapshot
}
