use super::*;

/// The histogram to export at `target` grouping power, or `None` if it cannot
/// be produced.
///
/// Validated histograms can always be downsampled to a lower grouping power.
/// Keep conversion failures nonfatal as a defensive measure: histogram serde
/// validates configurations and bucket counts, and metriken-exposition also
/// canonicalizes histograms before they reach this helper.
fn downsample_for_export(value: &histogram::Histogram, target: u8) -> Option<histogram::Histogram> {
    if target >= value.config().grouping_power() {
        return Some(value.clone());
    }
    value.downsample(target).ok()
}

/// Produces a snapshot from a previous and current snapshot
#[allow(deprecated)] // TODO: migrate from Histogram::percentiles to SampleQuantiles trait
pub fn snapshot(
    config: &Config,
    mut previous: Snapshot,
    mut current: Snapshot,
    latency: Duration,
) -> SnapshotV2 {
    let duration = current.duration().unwrap_or(latency);

    let mut snapshot = SnapshotV2 {
        systemtime: current.systemtime(),
        duration,
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

        snapshot
            .counters
            .push(Counter::new(name, curr.value, metadata).with_window(curr.window))
    }

    for curr in current.gauges() {
        let mut metadata = curr.metadata.clone();

        // the real metric name is encoded in the metadata
        let name = if let Some(name) = metadata.remove("metric") {
            name.to_string()
        } else {
            continue;
        };

        snapshot
            .gauges
            .push(Gauge::new(name, curr.value, metadata).with_window(curr.window))
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

            if let Ok(Some(percentiles)) = delta.percentiles(crate::common::DEFAULT_PERCENTILES) {
                for (percentile, value) in percentiles.into_iter().map(|(p, b)| (p, b.end())) {
                    if let Ok(value) = value.try_into() {
                        let mut metadata = metadata.clone();
                        metadata.insert("percentile".to_string(), percentile.to_string());

                        // Percentile summaries are computed from the delta between two
                        // snapshots, not a direct metric read — no single acquisition
                        // window applies.
                        snapshot
                            .gauges
                            .push(Gauge::new(name.clone(), value, metadata))
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

            // Dropped rather than fatal when it cannot be downsampled; see
            // `downsample_for_export`.
            let value = if let Some(v) =
                downsample_for_export(&curr.value, config.prometheus().histogram_grouping_power())
            {
                v
            } else {
                continue;
            };

            snapshot
                .histograms
                .push(Histogram::new(name, value, metadata).with_window(curr.window))
        }
    }

    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_exposition::{Histogram as ExpoHistogram, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn expo_histogram(value: histogram::Histogram) -> ExpoHistogram {
        let mut metadata = HashMap::new();
        metadata.insert("metric".to_string(), "latency".to_string());
        ExpoHistogram::new("latency".to_string(), value, metadata)
    }

    fn snapshot_with(h: ExpoHistogram) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH,
            duration: Duration::from_secs(1),
            metadata: HashMap::new(),
            counters: Vec::new(),
            gauges: Vec::new(),
            histograms: vec![h],
        })
    }

    #[test]
    fn malformed_histograms_are_rejected_when_decoding_agent_snapshots() {
        let wire = serde_json::to_value(snapshot_with(expo_histogram(
            histogram::Histogram::new(6, 16).unwrap(),
        )))
        .unwrap();
        let encoded = rmp_serde::to_vec_named(&wire).unwrap();
        assert!(rmp_serde::from_slice::<Snapshot>(&encoded).is_ok());

        // Exercise the same MessagePack decoder used by the exporter. Invalid
        // histograms are rejected here, before snapshot/downsample processing.
        for (field, replacement) in [
            (
                "/histograms/0/value/config/max_value_power",
                serde_json::json!(6),
            ),
            ("/histograms/0/value/config/max", serde_json::json!(0)),
            ("/histograms/0/value/buckets", serde_json::json!([0])),
        ] {
            let mut malformed = wire.clone();
            *malformed
                .pointer_mut(field)
                .expect("serialized histogram field") = replacement;
            let encoded = rmp_serde::to_vec_named(&malformed).unwrap();
            assert!(
                rmp_serde::from_slice::<Snapshot>(&encoded).is_err(),
                "invalid histogram field {field} must be rejected at decode"
            );
        }
    }

    #[test]
    fn well_formed_histograms_are_passed_through_or_downsampled() {
        let mut h = histogram::Histogram::new(6, 16).expect("valid config");
        h.add(100, 3).unwrap();
        h.add(1000, 4).unwrap();

        for target in [6, 8] {
            let same = downsample_for_export(&h, target).expect("no downsample needed");
            assert_eq!(same, h);
        }

        let coarser = downsample_for_export(&h, 4).expect("downsample must succeed");
        assert_eq!(coarser.config().grouping_power(), 4);
        assert_eq!(coarser.as_slice().iter().sum::<u64>(), 7);
    }

    #[test]
    fn decoded_histograms_are_exported_with_counts_and_metric_names() {
        let config: Config = toml::from_str(
            "[general]\n\
             [prometheus]\n\
             summaries = false\n\
             histograms = true\n\
             histogram_grouping_power = 4\n",
        )
        .expect("valid exporter config");
        let mut histogram = histogram::Histogram::new(6, 16).unwrap();
        histogram.add(100, 3).unwrap();
        histogram.add(1000, 4).unwrap();
        let wire = rmp_serde::to_vec(&snapshot_with(expo_histogram(histogram))).unwrap();
        let current: Snapshot = rmp_serde::from_slice(&wire).unwrap();
        let out = snapshot(&config, current.clone(), current, Duration::from_secs(1));

        assert_eq!(out.histograms.len(), 1);
        let exported = &out.histograms[0];
        assert_eq!(exported.name, "latency");
        assert_eq!(exported.value.config().grouping_power(), 4);
        assert_eq!(exported.value.as_slice().iter().sum::<u64>(), 7);
    }
}
