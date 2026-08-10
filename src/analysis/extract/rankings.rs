//! Top resource consumers per domain (cpu/memory/io/network), absorbed from
//! the orphaned `src/mcp/resource_usage.rs` prototype: per-series mean/max
//! over a range query, share-of-total, deterministic ordering.
//!
//! `memory` is empty in v1: current samplers expose only system-wide memory
//! gauges (no per-cgroup memory metrics), so there is no per-consumer
//! attribution to rank.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use metriken_query::{MetricsSource, QueryResult};

use super::features::finite_or_zero;
use crate::analysis::record::{Consumer, Rankings};

/// One query-result series: its label set and (timestamp, value) points.
pub(crate) type LabeledSeries = (HashMap<String, String>, Vec<(f64, f64)>);

/// (domain, base metric, unit, range query) — the base metric name feeds the
/// top-consumer promotion rule in the orchestrator.
pub(crate) const DOMAIN_QUERIES: &[(&str, &str, &str, &str)] = &[
    (
        "cpu",
        "cpu_usage",
        "cores",
        "sum by(id) (irate(cpu_usage[1m])) / 1e9",
    ),
    (
        "cpu",
        "cgroup_cpu_usage",
        "cores",
        "sum by(name) (irate(cgroup_cpu_usage[1m])) / 1e9",
    ),
    (
        "io",
        "blockio_operations",
        "ops/s",
        "sum by(op) (irate(blockio_operations[1m]))",
    ),
    (
        "io",
        "blockio_bytes",
        "bytes/s",
        "sum by(op) (irate(blockio_bytes[1m]))",
    ),
    (
        "network",
        "network_bytes",
        "bytes/s",
        "sum by(direction) (irate(network_bytes[1m]))",
    ),
    (
        "network",
        "network_packets",
        "packets/s",
        "sum by(direction) (irate(network_packets[1m]))",
    ),
];

const TOP_N: usize = 10;

/// Preferred label keys for a consumer's display name, in order.
const NAME_KEYS: &[&str] = &["name", "id", "op", "direction", "interface", "cpu"];

fn consumer_name(labels: &HashMap<String, String>) -> Option<String> {
    for key in NAME_KEYS {
        if let Some(v) = labels.get(*key) {
            return Some(v.clone());
        }
    }
    // fall back to a self-describing key=value join of the lexicographically-smallest non-dunder key
    labels
        .iter()
        .filter(|(k, _)| !k.starts_with("__"))
        .min_by(|a, b| a.0.cmp(b.0))
        .map(|(k, v)| format!("{k}={v}"))
}

/// Rank one query's series: per-series avg/max over finite values, share of
/// the total average, sorted by avg desc with name tiebreak, truncated to
/// `top_n` AFTER shares are computed (shares describe all series).
pub(crate) fn rank_consumers(
    series: &[LabeledSeries],
    metric: &str,
    unit: &str,
    top_n: usize,
) -> Vec<Consumer> {
    let mut consumers: Vec<Consumer> = Vec::new();
    let mut total = 0.0f64;
    for (labels, points) in series {
        let vals: Vec<f64> = points
            .iter()
            .map(|(_, v)| *v)
            .filter(|v| v.is_finite())
            .collect();
        if vals.is_empty() {
            continue;
        }
        let Some(name) = consumer_name(labels) else {
            continue;
        };
        let avg = vals.iter().sum::<f64>() / vals.len() as f64;
        let max = vals.iter().fold(f64::MIN, |a, b| a.max(*b));
        total += avg;
        consumers.push(Consumer {
            name,
            metric: metric.to_string(),
            unit: unit.to_string(),
            labels: labels
                .iter()
                .filter(|(k, _)| !k.starts_with("__"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<String, String>>(),
            avg_usage: finite_or_zero(avg),
            max_usage: finite_or_zero(max),
            percent_of_total: 0.0,
        });
    }
    if total > 0.0 {
        for c in &mut consumers {
            c.percent_of_total = finite_or_zero(c.avg_usage / total * 100.0);
        }
    }
    consumers.sort_by(|a, b| {
        b.avg_usage
            .total_cmp(&a.avg_usage)
            .then_with(|| a.name.cmp(&b.name))
    });
    consumers.truncate(top_n);
    consumers
}

/// Sort a domain bucket: group by source metric, largest-first within each
/// group, name tiebreak. NO truncation — the per-query cap in
/// `rank_consumers` is the only cap, so a bucket holds at most TOP_N
/// consumers per source metric. (The slice type enforces that: a `&mut [_]`
/// cannot be truncated.)
pub(crate) fn sort_domain_bucket(bucket: &mut [Consumer]) {
    bucket.sort_by(|a, b| {
        a.metric
            .cmp(&b.metric)
            .then_with(|| b.avg_usage.total_cmp(&a.avg_usage))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Run all domain queries. Returns the rankings plus the set of base metric
/// names that produced at least one consumer (feeds `top_consumer`
/// promotion). Queries that fail or return non-matrix results contribute
/// nothing — the domain just stays empty.
pub(crate) fn build_rankings(data: &dyn MetricsSource) -> (Rankings, BTreeSet<String>) {
    let mut rankings = Rankings {
        cpu: Vec::new(),
        memory: Vec::new(),
        io: Vec::new(),
        network: Vec::new(),
    };
    let Some((start, end)) = data.time_range() else {
        return (rankings, BTreeSet::new());
    };
    let step = super::guarded_step(data);
    for (domain, base_metric, unit, query) in DOMAIN_QUERIES {
        let Ok(QueryResult::Matrix { result }) = data.query_range(query, start, end, step) else {
            continue;
        };
        let series: Vec<LabeledSeries> = result.into_iter().map(|s| (s.metric, s.values)).collect();
        let consumers = rank_consumers(&series, base_metric, unit, TOP_N);
        if consumers.is_empty() {
            continue;
        }
        let bucket = match *domain {
            "cpu" => &mut rankings.cpu,
            "io" => &mut rankings.io,
            "network" => &mut rankings.network,
            "memory" => &mut rankings.memory,
            _ => continue,
        };
        bucket.extend(consumers);
    }
    // Per-metric grouping: sort each bucket by (metric asc, avg_usage desc, name asc),
    // keeping units segregated and ordering meaningful within each group.
    for bucket in [
        &mut rankings.cpu,
        &mut rankings.memory,
        &mut rankings.io,
        &mut rankings.network,
    ] {
        sort_domain_bucket(bucket);
    }
    // Derive ranked_metrics from surviving bucket contents.
    let mut ranked_metrics = BTreeSet::new();
    for bucket in [
        &rankings.cpu,
        &rankings.memory,
        &rankings.io,
        &rankings.network,
    ] {
        for consumer in bucket {
            ranked_metrics.insert(consumer.metric.clone());
        }
    }
    (rankings, ranked_metrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn series(name: &str, key: &str, vals: &[f64]) -> LabeledSeries {
        let mut labels = HashMap::new();
        labels.insert(key.to_string(), name.to_string());
        (
            labels,
            vals.iter()
                .enumerate()
                .map(|(i, v)| (i as f64, *v))
                .collect(),
        )
    }

    #[test]
    fn consumers_ranked_with_share_and_tiebreak() {
        let input = vec![
            series("b", "id", &[2.0, 2.0]),
            series("a", "id", &[2.0, 2.0]),
            series("c", "id", &[6.0, 6.0]),
        ];
        let consumers = rank_consumers(&input, "cpu_usage", "cores", 10);
        assert_eq!(consumers.len(), 3);
        // c first (avg 6), then a/b tie broken by name
        assert_eq!(consumers[0].name, "c");
        assert_eq!(consumers[1].name, "a");
        assert_eq!(consumers[2].name, "b");
        assert!((consumers[0].percent_of_total - 60.0).abs() < 1e-9);
        assert!((consumers[0].avg_usage - 6.0).abs() < 1e-9);
        assert!((consumers[0].max_usage - 6.0).abs() < 1e-9);
        // labels carried over as sorted BTreeMap
        assert_eq!(consumers[0].labels.get("id").map(String::as_str), Some("c"));
        // metric and unit are set
        assert_eq!(consumers[0].metric, "cpu_usage");
        assert_eq!(consumers[0].unit, "cores");
    }

    #[test]
    fn consumers_truncated_after_share_computed() {
        let input = vec![series("a", "id", &[9.0]), series("b", "id", &[1.0])];
        let consumers = rank_consumers(&input, "blockio_bytes", "bytes/s", 1);
        assert_eq!(consumers.len(), 1);
        // 90% of ALL series, not 100% of the retained one
        assert!((consumers[0].percent_of_total - 90.0).abs() < 1e-9);
        // metric and unit are set
        assert_eq!(consumers[0].metric, "blockio_bytes");
        assert_eq!(consumers[0].unit, "bytes/s");
    }

    #[test]
    fn nan_series_skipped_and_names_synthesized() {
        let mut unlabeled = HashMap::new();
        unlabeled.insert("__name__".to_string(), "x".to_string());
        let input = vec![
            (unlabeled, vec![(0.0, f64::NAN)]),
            series("a", "name", &[1.0]),
        ];
        let consumers = rank_consumers(&input, "network_bytes", "bytes/s", 10);
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].name, "a");
        assert_eq!(consumers[0].metric, "network_bytes");
        assert_eq!(consumers[0].unit, "bytes/s");
    }

    #[test]
    fn domain_bucket_keeps_all_metric_groups() {
        // two metrics, 12 consumers each (rank_consumers caps at TOP_N=10),
        // merged into one bucket: both groups must survive in full
        let many = |prefix: &str| -> Vec<LabeledSeries> {
            (0..12)
                .map(|i| series(&format!("{prefix}{i:02}"), "id", &[100.0 - i as f64]))
                .collect()
        };
        let a = rank_consumers(&many("cg"), "cgroup_cpu_usage", "cores", TOP_N);
        let b = rank_consumers(&many("core"), "cpu_usage", "cores", TOP_N);
        assert_eq!(a.len(), 10);
        assert_eq!(b.len(), 10);
        let mut bucket: Vec<Consumer> = Vec::new();
        bucket.extend(a);
        bucket.extend(b);
        sort_domain_bucket(&mut bucket);
        assert_eq!(bucket.len(), 20, "no cross-metric truncation");
        // cgroup group first (metric asc), then per-core group, each avg-desc
        assert_eq!(bucket[0].metric, "cgroup_cpu_usage");
        assert_eq!(bucket[10].metric, "cpu_usage");
        assert_eq!(
            bucket.iter().filter(|c| c.metric == "cpu_usage").count(),
            10
        );
    }
}
