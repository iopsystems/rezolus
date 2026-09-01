use metriken_exposition::{Counter, Gauge, Histogram as SnapshotHistogram, Snapshot, SnapshotV2};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tracing::warn;

/// Converts Prometheus text format responses into SnapshotV2 objects that can be
/// serialized as msgpack and processed by the existing parquet conversion pipeline.
///
/// Maintains a stable mapping from (metric_name, labels) to numeric IDs across
/// scrapes within a recording session, ensuring consistent parquet column identity.
pub struct PrometheusConverter {
    metric_ids: HashMap<MetricKey, usize>,
    next_id: usize,
    descriptions: HashMap<String, String>,
    source: Option<String>,
    endpoint: Option<String>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct MetricKey {
    name: String,
    labels: Vec<(String, String)>,
}

impl PrometheusConverter {
    pub fn with_provenance(source: String, endpoint: String) -> Self {
        Self {
            metric_ids: HashMap::new(),
            next_id: 0,
            descriptions: HashMap::new(),
            source: Some(source),
            endpoint: Some(endpoint),
        }
    }

    /// Returns the accumulated metric descriptions from all scrapes.
    pub fn descriptions(&self) -> &HashMap<String, String> {
        &self.descriptions
    }

    fn get_or_assign_id(&mut self, name: &str, labels: &[(String, String)]) -> String {
        let key = MetricKey {
            name: name.to_string(),
            labels: labels.to_vec(),
        };
        if let Some(id) = self.metric_ids.get(&key) {
            return id.to_string();
        }
        let id = self.next_id;
        self.next_id += 1;
        self.metric_ids.insert(key, id);
        id.to_string()
    }

    fn build_metadata(&self, name: &str, labels: &[(String, String)]) -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        metadata.insert("metric".to_string(), name.to_string());
        for (k, v) in labels {
            metadata.insert(k.clone(), v.clone());
        }
        if let Some(ref source) = self.source {
            metadata.insert("source".to_string(), source.clone());
        }
        if let Some(ref endpoint) = self.endpoint {
            metadata.insert("endpoint".to_string(), endpoint.clone());
        }
        metadata
    }

    /// Convert one scrape, bracketed by the round trip that produced it.
    ///
    /// `request_ns`/`response_ns` are when the request went out and when the
    /// response finished arriving. Every value in `text` was read by the
    /// exporter somewhere inside that interval, and nothing here can narrow it
    /// further: exposition carries no acquisition instant, and the one
    /// timestamp it may carry means something else entirely (see
    /// [`sample_window`]).
    pub fn convert(&mut self, text: &str, request_ns: u64, response_ns: u64) -> Snapshot {
        let fetch_ns = response_ns;
        let sanitized = sanitize_metric_names(text);
        let lines = sanitized.lines().map(|l| Ok(l.to_string()));
        let fetch_time = chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(fetch_ns as i64);
        let scrape = match prometheus_parse::Scrape::parse_at(lines, fetch_time) {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to parse prometheus metrics: {e}");
                return empty_snapshot();
            }
        };

        // Accumulate HELP descriptions across scrapes
        for (name, doc) in &scrape.docs {
            self.descriptions
                .entry(name.clone())
                .or_insert_with(|| doc.clone());
        }

        let mut counters = Vec::new();
        let mut gauges = Vec::new();
        let mut histograms = Vec::new();

        // One window for the whole scrape: every sample in it was read by the
        // same request/response, so they share an acquisition instant.
        let window = sample_window(request_ns, response_ns);

        for sample in scrape.samples {
            let mut labels: Vec<(String, String)> = sample
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            labels.sort();

            match sample.value {
                prometheus_parse::Value::Counter(v) => {
                    if !v.is_finite() {
                        continue;
                    }
                    let id = self.get_or_assign_id(&sample.metric, &labels);
                    counters.push(
                        Counter::new(id, v as u64, self.build_metadata(&sample.metric, &labels))
                            .with_window(window),
                    );
                }
                prometheus_parse::Value::Gauge(v) => {
                    if !v.is_finite() {
                        continue;
                    }
                    let id = self.get_or_assign_id(&sample.metric, &labels);
                    gauges.push(
                        Gauge::new(id, v as i64, self.build_metadata(&sample.metric, &labels))
                            .with_window(window),
                    );
                }
                prometheus_parse::Value::Untyped(v) => {
                    if !v.is_finite() {
                        continue;
                    }
                    // _total, _sum, and _count are monotonically increasing
                    // by Prometheus convention, so store them as counters.
                    // _sum is particularly useful: rate(_sum) / rate(_count)
                    // gives the true mean for comparison against approximated
                    // histogram percentiles.
                    let id = self.get_or_assign_id(&sample.metric, &labels);
                    let metadata = self.build_metadata(&sample.metric, &labels);
                    if sample.metric.ends_with("_total")
                        || sample.metric.ends_with("_sum")
                        || sample.metric.ends_with("_count")
                    {
                        counters.push(Counter::new(id, v as u64, metadata).with_window(window));
                    } else {
                        gauges.push(Gauge::new(id, v as i64, metadata).with_window(window));
                    }
                }
                prometheus_parse::Value::Histogram(ref buckets) => {
                    if let Some((h, metadata)) = convert_histogram(
                        buckets,
                        &sample.metric,
                        &labels,
                        self.source.as_deref(),
                        self.endpoint.as_deref(),
                    ) {
                        let id = self.get_or_assign_id(&sample.metric, &labels);
                        histograms
                            .push(SnapshotHistogram::new(id, h, metadata).with_window(window));
                    }
                }
                prometheus_parse::Value::Summary(ref quantiles) => {
                    for quantile in quantiles {
                        if !quantile.count.is_finite() {
                            continue;
                        }
                        let q = quantile.quantile.to_string();
                        let mut q_labels = labels.clone();
                        q_labels.push(("quantile".to_string(), q));
                        q_labels.sort();
                        let id = self.get_or_assign_id(&sample.metric, &q_labels);
                        gauges.push(
                            Gauge::new(
                                id,
                                quantile.count as i64,
                                self.build_metadata(&sample.metric, &q_labels),
                            )
                            .with_window(window),
                        );
                    }
                }
            }
        }

        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::now(),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges,
            histograms,
        })
    }
}

/// Convert Prometheus cumulative histogram buckets into a histogram::Histogram.
///
/// Uses the upper bound (le) of each bucket as the representative value and
/// computes per-bucket delta counts from the cumulative Prometheus counts.
///
/// For `_seconds` metrics, le values are multiplied by 1e9 to convert to
/// nanoseconds, matching Rezolus's native histogram unit. Other metrics use a
/// generic power-of-10 scale that makes the smallest le boundary >= 1.
fn convert_histogram(
    buckets: &[prometheus_parse::HistogramCount],
    metric_name: &str,
    labels: &[(String, String)],
    source: Option<&str>,
    endpoint: Option<&str>,
) -> Option<(histogram::Histogram, HashMap<String, String>)> {
    // Filter to finite boundaries only (+Inf cannot be represented)
    let finite_buckets: Vec<_> = buckets
        .iter()
        .filter(|b| b.less_than.is_finite() && b.count.is_finite())
        .collect();

    if finite_buckets.is_empty() {
        return None;
    }

    // For _seconds histograms, convert to nanoseconds to match Rezolus convention.
    // Otherwise, use a generic scale that preserves precision.
    let scale = if metric_name.ends_with("_seconds") {
        1e9
    } else {
        compute_generic_scale(&finite_buckets)
    };

    // max_value_power must cover the largest scaled value
    let max_scaled = finite_buckets
        .iter()
        .map(|b| (b.less_than * scale) as u64)
        .max()
        .unwrap_or(1);
    let max_value_power = if max_scaled == 0 {
        8
    } else {
        ((max_scaled as f64).log2().ceil() as u8 + 1).clamp(8, 64)
    };

    let grouping_power: u8 = 7;

    let mut h = histogram::Histogram::new(grouping_power, max_value_power).ok()?;

    // Convert cumulative counts to deltas and add to histogram
    let mut prev_count = 0u64;
    for bucket in &finite_buckets {
        let cum_count = bucket.count as u64;
        let delta = cum_count.saturating_sub(prev_count);
        if delta > 0 {
            let value = (bucket.less_than * scale) as u64;
            let _ = h.add(value, delta);
        }
        prev_count = cum_count;
    }

    let mut metadata = HashMap::new();
    metadata.insert("metric".to_string(), metric_name.to_string());
    for (k, v) in labels {
        metadata.insert(k.clone(), v.clone());
    }
    if let Some(s) = source {
        metadata.insert("source".to_string(), s.to_string());
    }
    if let Some(e) = endpoint {
        metadata.insert("endpoint".to_string(), e.to_string());
    }
    metadata.insert("grouping_power".to_string(), grouping_power.to_string());
    metadata.insert("max_value_power".to_string(), max_value_power.to_string());

    Some((h, metadata))
}

/// Compute a power-of-10 scale factor that makes the smallest positive le
/// boundary >= 1, preserving precision when converting float boundaries to u64.
fn compute_generic_scale(buckets: &[&prometheus_parse::HistogramCount]) -> f64 {
    let min_le = buckets
        .iter()
        .map(|b| b.less_than)
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);

    if min_le >= 1.0 || min_le == f64::INFINITY {
        return 1.0;
    }

    let mut scale = 1.0;
    while min_le * scale < 1.0 {
        scale *= 10.0;
    }
    scale
}

/// Replace colons with underscores in metric names.
///
/// The prometheus-parse crate uses `\w+` regexes for metric names, which
/// doesn't include colons. Prometheus allows colons in metric names (commonly
/// used by recording rules and namespaced exporters like vLLM), so we
/// normalize them to underscores before parsing.
///
/// Only the metric name portion of each line is modified - label values and
/// HELP descriptions are left untouched.
fn sanitize_metric_names(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            // For comment lines (HELP/TYPE), replace colons in the metric
            // name token only (the word after HELP/TYPE keyword)
            if let Some(rest) = trimmed
                .strip_prefix("# HELP ")
                .or(trimmed.strip_prefix("# TYPE "))
            {
                let prefix = &trimmed[..trimmed.len() - rest.len()];
                // The metric name is the first token
                let name_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                let name = &rest[..name_end];
                let after = &rest[name_end..];
                result.push_str(prefix);
                result.push_str(&name.replace(':', "_"));
                result.push_str(after);
            } else {
                result.push_str(trimmed);
            }
        } else {
            // Sample line: metric_name{labels} value [timestamp]
            // Replace colons only in the metric name (before '{' or whitespace)
            let name_end = trimmed
                .find(|c: char| c == '{' || c.is_whitespace())
                .unwrap_or(trimmed.len());
            let name = &trimmed[..name_end];
            let after = &trimmed[name_end..];
            result.push_str(&name.replace(':', "_"));
            result.push_str(after);
        }
        result.push('\n');
    }
    result
}

/// The acquisition window for every sample in one scrape.
///
/// Derived from `fetch_ns` — the recorder's own clock for this tick — and
/// deliberately NOT from `sample.timestamp`.
///
/// Prometheus exposition allows an optional trailing timestamp in milliseconds
/// since epoch, meant as a federation/pushgateway staleness marker.
/// `Scrape::parse_at` puts that value in `sample.timestamp` when present and
/// the fetch instant otherwise, so using it meant one recording silently
/// carried two different semantics — and, worse, took the exporter's claim at
/// face value: `m_total 3 1000` yielded a window beginning one second after
/// the Unix epoch, decades before the row holding it. The window offset is
/// stored relative to the row timestamp, so that is not a merely-wide bound
/// but a ~56-year error in the operand `rate()` prices its uncertainty from.
/// Any exporter that emits timestamps (pushgateway, federation) wrote that.
///
/// The window is the real round trip, `[request_sent, response_received]`. It
/// was zero-width — a whole scrape asserted to have been read at an instant,
/// which is the lie the all-sampler-observation-windows arc exists to kill.
/// Every value in a response was read by the exporter somewhere inside the
/// round trip and nothing here can say where, so that interval is the honest
/// bound: `rate()` prices its uncertainty from a bracket that actually
/// contains the reading rather than one asserted to be exact.
///
/// **A caching exporter under-states this, and that is still an improvement.**
/// If an exporter serves values it computed before the request arrived, the
/// true acquisition instant is earlier than `request_sent` and the real
/// uncertainty is wider than what is recorded. Nothing observable from the
/// client distinguishes that case — exposition carries no acquisition instant
/// — so the recorded window is a lower bound on the uncertainty rather than a
/// complete account of it. A zero-width window was a lower bound too, and a
/// far worse one: it claimed no uncertainty at all.
fn sample_window(request_ns: u64, response_ns: u64) -> Option<metriken::Window> {
    // Defensive: a non-monotonic wall clock (NTP step, VM migration) can make
    // the response read EARLIER than the request. A window whose end precedes
    // its begin would give `rate()` a saturating-to-zero width, so collapse it
    // to the instant we are surest of instead.
    if response_ns < request_ns {
        return Some(metriken::Window::new(response_ns, response_ns));
    }
    Some(metriken::Window::new(request_ns, response_ns))
}

fn empty_snapshot() -> Snapshot {
    Snapshot::V2(SnapshotV2 {
        systemtime: SystemTime::now(),
        duration: Duration::ZERO,
        metadata: HashMap::new(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An exporter's own trailing timestamp must NOT become the acquisition
    /// window.
    ///
    /// Prometheus exposition allows an optional trailing timestamp in
    /// milliseconds since epoch, meant as a federation/pushgateway staleness
    /// marker. Taking it at face value made the window say when the *exporter*
    /// claims the value was computed — on a scale where `m_total 3 1000` means
    /// one second after the Unix epoch, decades before the recording holding
    /// it. This test asserted exactly that until it was corrected.
    ///
    /// The window has to come from OUR clock around OUR fetch, because that is
    /// the only interval we actually observed. It is also what makes one
    /// recording carry one semantics: before this, a scrape with timestamps
    /// and a scrape without silently mixed two.
    #[test]
    fn an_embedded_timestamp_is_not_the_window() {
        let mut conv = PrometheusConverter::with_provenance("svc".into(), "http://x".into());
        let (request_ns, response_ns) = (5_000_000_000u64, 5_002_000_000u64);
        let text = "m_total 3 1000\n";
        let Snapshot::V2(s) = conv.convert(text, request_ns, response_ns) else {
            panic!()
        };
        let w = s.counters[0].window.expect("window set");
        assert_eq!(
            (w.begin_ns, w.end_ns),
            (request_ns, response_ns),
            "the window is our round trip, not the exporter's 1000 ms epoch stamp"
        );
    }

    /// The offset is stored relative to the row timestamp, so an epoch-anchored
    /// window is not merely wide — it is a ~56-year error in the operand
    /// `rate()` prices its uncertainty from.
    #[test]
    fn an_embedded_timestamp_cannot_predate_the_row_it_describes() {
        let mut conv = PrometheusConverter::with_provenance("svc".into(), "http://x".into());
        // A realistic recording clock: 2026-ish, not 1970.
        let fetch_ns = 1_780_000_000_000_000_000u64;
        let Snapshot::V2(s) = conv.convert("m_total 3 1000\n", fetch_ns, fetch_ns + 2_000_000)
        else {
            panic!()
        };
        let w = s.counters[0].window.expect("window set");
        assert!(
            w.begin_ns >= fetch_ns.saturating_sub(1),
            "a window beginning {} against a row at {fetch_ns} would be decades of \
             fabricated uncertainty",
            w.begin_ns
        );
    }

    #[test]
    fn absent_timestamp_falls_back_to_fetch_time() {
        let mut conv = PrometheusConverter::with_provenance("svc".into(), "http://x".into());
        let (request_ns, response_ns) = (5_000_000_000u64, 5_002_000_000u64);
        let text = "m_total 3\n";
        let Snapshot::V2(s) = conv.convert(text, request_ns, response_ns) else {
            panic!()
        };
        let w = s.counters[0].window.expect("window set");
        assert_eq!((w.begin_ns, w.end_ns), (request_ns, response_ns));
    }

    /// A scrape is one acquisition, and its honest bracket is the round trip.
    ///
    /// This was `Window::new(ns, ns)` — a whole scrape asserted to have been
    /// read at an instant, which is the claim
    /// `docs/journal/2026-07-10-all-sampler-observation-windows.md` calls the
    /// lie the arc kills. A zero-width window tells `rate()` there is no
    /// uncertainty to price; the round trip tells it the truth we can actually
    /// observe.
    #[test]
    fn every_value_in_a_scrape_carries_the_round_trip_as_its_window() {
        let mut conv = PrometheusConverter::with_provenance("svc".into(), "http://x".into());
        let (request_ns, response_ns) = (10_000_000_000u64, 10_045_000_000u64);
        let text = "\
# TYPE a_total counter
a_total 1
# TYPE b gauge
b 2
";
        let Snapshot::V2(s) = conv.convert(text, request_ns, response_ns) else {
            panic!()
        };
        assert!(!s.counters.is_empty() && !s.gauges.is_empty(), "fixture");
        for w in s
            .counters
            .iter()
            .map(|c| c.window)
            .chain(s.gauges.iter().map(|g| g.window))
        {
            let w = w.expect("every value carries a window");
            assert_eq!((w.begin_ns, w.end_ns), (request_ns, response_ns));
            assert!(w.width_ns() > 0, "a scrape is not an instant");
        }
    }

    /// A wall clock that steps backwards mid-scrape (NTP, a VM migration) must
    /// not produce a window whose end precedes its begin: the width saturates
    /// to zero, which would silently re-assert the very claim this replaced.
    #[test]
    fn a_backwards_clock_collapses_rather_than_inverting() {
        let w = sample_window(9_000, 8_000).unwrap();
        assert_eq!((w.begin_ns, w.end_ns), (8_000, 8_000));
    }
}
