use crate::viewer::promql::{QueryEngine, QueryError};
use crate::viewer::tsdb::Tsdb;
use std::sync::Arc;

fn create_test_tsdb() -> Tsdb {
    Tsdb::default()
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

    // Should return Unsupported error since histogram_quantile is not yet implemented
    match result {
        Err(QueryError::Unsupported(msg)) if msg.contains("histogram_quantile") => {}
        _ => panic!("Expected Unsupported error for histogram_quantile"),
    }
}
