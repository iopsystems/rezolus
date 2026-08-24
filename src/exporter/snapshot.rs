use super::*;

/// The histogram to export at `target` grouping power, or `None` if it cannot
/// be produced.
///
/// `None` means drop the metric — the same choice the summary path makes when
/// `wrapping_sub` fails. This used to be an `unwrap`, and with a well-formed
/// histogram it could not fail: `downsample` rebuilds the config as
/// `Config::new(target, max_value_power)`, which only rejects
/// `target >= max_value_power`, and we only downsample when
/// `target < grouping_power`, which a valid config keeps below
/// `max_value_power`. Reaching the failure needs a config that violates that
/// invariant, and metriken-exposition canonicalizes decoded histograms through
/// `from_buckets` — dropping the ones it cannot rebuild — before they ever get
/// here.
///
/// So the panic was unreachable in practice. What made it safe, though, was an
/// invariant enforced in another crate, at a distance, with nothing at this
/// call site saying so; an always-on exporter should not stake the process on
/// that holding forever.
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

    /// A histogram whose config violates the invariant the validating
    /// constructors enforce (`grouping_power < max_value_power`).
    ///
    /// It has to be built by deserializing hand-written JSON: every public
    /// constructor rejects it, which is exactly why a decoded snapshot is the
    /// only way such a value ever appears. `histogram::Histogram`'s
    /// `Deserialize` is a plain derive over raw fields, so it performs no
    /// validation — the same door a malformed msgpack snapshot would come
    /// through.
    fn histogram_with_impossible_config() -> histogram::Histogram {
        // grouping_power 10 >= max_value_power 6, which `Config::new` rejects
        // with `MaxPowerTooLow`. Bucket count matches the declared geometry so
        // that only the power relationship is wrong.
        let json = r#"{
            "config": {
                "max": 63,
                "grouping_power": 10,
                "max_value_power": 6,
                "cutoff_power": 17,
                "cutoff_value": 131072,
                "lower_bin_count": 1024,
                "upper_bin_divisions": 1024,
                "upper_bin_count": 0
            },
            "buckets": [0]
        }"#;
        serde_json::from_str(json).expect("the derive performs no validation")
    }

    fn expo_histogram(value: histogram::Histogram) -> ExpoHistogram {
        let mut metadata = HashMap::new();
        metadata.insert("metric".to_string(), "malformed".to_string());
        ExpoHistogram::new("malformed".to_string(), value, metadata)
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

    /// The downsample decision drops a histogram it cannot convert.
    ///
    /// Calls the helper directly, because the end-to-end path cannot reach it:
    /// canonicalization drops a malformed histogram first, which is exactly why
    /// the `unwrap` this replaced never fired in practice. Testing the decision
    /// in isolation is the only way to cover the branch that exists for when
    /// that outer guarantee stops holding.
    #[test]
    fn a_histogram_that_cannot_be_downsampled_is_not_exported() {
        let bad = histogram_with_impossible_config();
        // Target below its grouping power, so the downsample branch is taken;
        // `Config::new(7, 6)` then fails with `MaxPowerTooLow`.
        assert_eq!(
            downsample_for_export(&bad, 7),
            None,
            "a histogram that cannot be downsampled must be dropped rather \
             than unwrapped"
        );
    }

    /// A histogram already at or below the target is passed through untouched,
    /// and one above it is genuinely downsampled — so the drop above is the
    /// failure path, not the only path.
    #[test]
    fn well_formed_histograms_are_passed_through_or_downsampled() {
        let h = histogram::Histogram::new(6, 16).expect("valid config");

        let same = downsample_for_export(&h, 6).expect("no downsample needed");
        assert_eq!(same.config().grouping_power(), 6);

        let coarser = downsample_for_export(&h, 4).expect("downsample must succeed");
        assert_eq!(coarser.config().grouping_power(), 4);
    }

    /// A malformed histogram cannot take the exporter down, end to end.
    ///
    /// This passes with or without the `unwrap` fix, and that is the point
    /// worth being precise about: the malformed histogram never reaches the
    /// downsample, because metriken-exposition drops it while canonicalizing
    /// on the way out of `histograms()`. So this test covers the OUTER layer.
    ///
    /// It earns its place because the exporter is always on and scrapes
    /// whatever an agent sends: if that canonicalization is ever relaxed,
    /// moved, or bypassed by a new decode path, this fails here instead of a
    /// production exporter aborting mid-scrape. The inner layer — the drop
    /// itself — is covered by
    /// [`a_histogram_that_cannot_be_downsampled_is_not_exported`].
    #[test]
    fn a_histogram_that_cannot_be_downsampled_is_dropped_not_fatal() {
        // histograms on, and a target grouping power below the malformed
        // histogram's, so the downsample branch is the one taken.
        let config: Config = toml::from_str(
            "[general]\n\
             [prometheus]\n\
             histograms = true\n\
             histogram_grouping_power = 7\n",
        )
        .expect("valid exporter config");

        let previous = snapshot_with(expo_histogram(histogram_with_impossible_config()));
        let current = snapshot_with(expo_histogram(histogram_with_impossible_config()));

        let out = snapshot(&config, previous, current, Duration::from_secs(1));

        assert!(
            out.histograms.is_empty(),
            "a histogram that cannot be downsampled must be dropped, not \
             exported half-formed"
        );
    }
}
