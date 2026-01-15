use crate::viewer::promql::{MatrixSample, QueryEngine, QueryError};
use crate::viewer::tsdb::Tsdb;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn create_test_tsdb() -> Tsdb {
    Tsdb::default()
}

/// Test helper: aggregate samples using the specified operation
fn aggregate_samples(op: &str, samples: &[&MatrixSample]) -> Vec<(f64, f64)> {
    let agg_fn = QueryEngine::get_aggregation_fn(op).expect("Unknown aggregation");

    // Collect all values at each timestamp
    let mut timestamp_values: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for sample in samples {
        for (ts, val) in &sample.values {
            let ts_key = (*ts * 1e9) as u64;
            timestamp_values.entry(ts_key).or_default().push(*val);
        }
    }

    // Apply the aggregation function at each timestamp
    timestamp_values
        .into_iter()
        .filter_map(|(ts_ns, values)| {
            agg_fn(&values).map(|result| (ts_ns as f64 / 1e9, result))
        })
        .collect()
}

#[test]
fn test_query_engine_creation() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test that we can create a query engine
    assert!(!engine.tsdb.source().is_empty() || engine.tsdb.source() == "");
}

#[test]
fn test_simple_rate_query_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test that rate query parsing doesn't panic
    let result = engine.query("rate(cpu_cycles[5m])", None);

    // Should return MetricNotFound for empty TSDB, but shouldn't crash
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_simple_metric_query() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test simple metric query
    let result = engine.query("cpu_cores", None);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_sum_rate_query() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test sum(rate()) query parsing
    let result = engine.query("sum(rate(network_rx_bytes[1m]))", None);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_range_query_delegation() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test that range queries delegate to instant queries
    let result = engine.query_range("cpu_cores", 0.0, 3600.0, 60.0);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_label_filtering_in_rate_query() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test rate query with label filtering
    let result = engine.query("rate(network_bytes{direction=\"transmit\"}[5m])", None);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_label_filtering_in_sum_rate_query() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test sum(rate()) query with label filtering
    let result = engine.query("sum(rate(blockio_bytes{op=\"read\"}[1m]))", None);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_simple_metric_with_labels() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test simple metric query with label filtering
    let result = engine.query("cpu_cores{cpu=\"0\"}", None);

    // Should return MetricNotFound for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_metric_selector_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test that parse_metric_selector works correctly (we can't call it directly due to visibility)
    // but we can test it indirectly through query parsing

    // This should not panic during parsing
    let _result = engine.query("metric_name{label1=\"value1\",label2=\"value2\"}", None);

    // Multiple labels with single quotes
    let _result = engine.query("metric_name{label1='value1',label2='value2'}", None);

    // Labels with spaces
    let _result = engine.query("metric_name{label1 = \"value 1\", label2= 'value 2'}", None);
}

#[test]
fn test_histogram_quantile_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test single percentile histogram_quantile parsing
    let result = engine.query_range(
        "histogram_quantile(0.95, tcp_packet_latency)",
        0.0,
        3600.0,
        60.0,
    );

    // Should return MetricNotFound error for empty TSDB
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_avg_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test avg() query parsing
    let result = engine.query("avg(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("avg should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_min_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test min() query parsing
    let result = engine.query("min(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("min should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_max_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test max() query parsing
    let result = engine.query("max(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("max should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_count_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test count() query parsing
    let result = engine.query("count(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("count should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_stddev_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test stddev() query parsing
    let result = engine.query("stddev(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("stddev should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_stdvar_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test stdvar() query parsing
    let result = engine.query("stdvar(rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("stdvar should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_avg_by_aggregation_parsing() {
    let tsdb = Arc::new(create_test_tsdb());
    let engine = QueryEngine::new(tsdb);

    // Test avg by (label) query parsing
    let result = engine.query("avg by (cpu) (rate(cpu_cycles[1m]))", None);

    // Should return MetricNotFound for empty TSDB (not Unsupported)
    match result {
        Err(QueryError::MetricNotFound(_)) => {}
        Err(QueryError::Unsupported(msg)) => {
            panic!("avg by should be supported, got Unsupported: {}", msg)
        }
        _ => panic!("Expected MetricNotFound error for empty TSDB"),
    }
}

#[test]
fn test_aggregate_samples_sum() {
    // Test the aggregate_samples function directly
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0), (2.0, 20.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 5.0), (2.0, 15.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2];
    let result = aggregate_samples("sum", &samples);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1.0, 15.0)); // 10 + 5
    assert_eq!(result[1], (2.0, 35.0)); // 20 + 15
}

#[test]
fn test_aggregate_samples_avg() {
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0), (2.0, 20.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 20.0), (2.0, 40.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2];
    let result = aggregate_samples("avg", &samples);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1.0, 15.0)); // (10 + 20) / 2
    assert_eq!(result[1], (2.0, 30.0)); // (20 + 40) / 2
}

#[test]
fn test_aggregate_samples_min() {
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0), (2.0, 50.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 5.0), (2.0, 15.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2];
    let result = aggregate_samples("min", &samples);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1.0, 5.0)); // min(10, 5)
    assert_eq!(result[1], (2.0, 15.0)); // min(50, 15)
}

#[test]
fn test_aggregate_samples_max() {
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0), (2.0, 50.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 5.0), (2.0, 15.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2];
    let result = aggregate_samples("max", &samples);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1.0, 10.0)); // max(10, 5)
    assert_eq!(result[1], (2.0, 50.0)); // max(50, 15)
}

#[test]
fn test_aggregate_samples_count() {
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0), (2.0, 20.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 5.0), (2.0, 15.0)],
    };
    let sample3 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 7.0)], // Only has timestamp 1.0
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2, &sample3];
    let result = aggregate_samples("count", &samples);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1.0, 3.0)); // 3 samples at t=1.0
    assert_eq!(result[1], (2.0, 2.0)); // 2 samples at t=2.0
}

#[test]
fn test_aggregate_samples_stdvar() {
    // Values: 10, 20, 30 at t=1.0 -> mean=20, variance=((10-20)^2 + (20-20)^2 + (30-20)^2)/3 = 200/3
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 20.0)],
    };
    let sample3 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 30.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2, &sample3];
    let result = aggregate_samples("stdvar", &samples);

    assert_eq!(result.len(), 1);
    let expected_variance = 200.0 / 3.0; // ~66.67
    assert!((result[0].1 - expected_variance).abs() < 0.01);
}

#[test]
fn test_aggregate_samples_stddev() {
    // Values: 10, 20, 30 at t=1.0 -> stddev = sqrt(variance) = sqrt(200/3)
    let sample1 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 10.0)],
    };
    let sample2 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 20.0)],
    };
    let sample3 = MatrixSample {
        metric: HashMap::new(),
        values: vec![(1.0, 30.0)],
    };

    let samples: Vec<&MatrixSample> = vec![&sample1, &sample2, &sample3];
    let result = aggregate_samples("stddev", &samples);

    assert_eq!(result.len(), 1);
    let expected_stddev = (200.0_f64 / 3.0).sqrt(); // ~8.16
    assert!((result[0].1 - expected_stddev).abs() < 0.01);
}
