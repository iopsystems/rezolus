use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use std::collections::HashMap;
use std::sync::Arc;

/// Result of resource usage analysis
#[derive(Debug)]
pub struct ResourceUsageResult {
    pub query: String,
    pub description: String,
    pub top_consumers: Vec<ResourceConsumer>,
    pub total_usage: f64,
    pub time_range: (f64, f64),
}

/// A resource consumer with usage statistics
#[derive(Debug, Clone)]
pub struct ResourceConsumer {
    pub labels: HashMap<String, String>,
    pub name: String,
    pub avg_usage: f64,
    pub max_usage: f64,
    pub min_usage: f64,
    pub percent_of_total: f64,
}

/// Analyze top resource consumers for a given query
pub fn analyze_resource_usage(
    engine: &Arc<QueryEngine>,
    tsdb: &Arc<Tsdb>,
    query: &str,
    description: &str,
    top_n: usize,
) -> Result<ResourceUsageResult, Box<dyn std::error::Error>> {
    // Get time range from TSDB
    let (start, end) = engine.get_time_range();
    let step = tsdb.interval();
    
    // Execute the query
    let result = engine.query_range(query, start, end, step)?;
    
    // Extract samples from the result
    use crate::viewer::promql::{QueryResult, Sample};
    
    let mut consumers = Vec::new();
    let mut total_sum = 0.0;
    
    match result {
        QueryResult::Matrix { result: matrix } => {
            for series in matrix {
                let values: Vec<f64> = series.values.iter()
                    .map(|(_, value)| *value)
                    .filter(|v| !v.is_nan())
                    .collect();
                
                if values.is_empty() {
                    continue;
                }
                
                let sum: f64 = values.iter().sum();
                let avg = sum / values.len() as f64;
                let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                
                // Extract a readable name from labels
                let name = extract_consumer_name(&series.metric);
                
                consumers.push(ResourceConsumer {
                    labels: series.metric.clone(),
                    name,
                    avg_usage: avg,
                    max_usage: max,
                    min_usage: min,
                    percent_of_total: 0.0, // Will calculate after sorting
                });
                
                total_sum += avg;
            }
        }
        QueryResult::Vector { result: vector } => {
            for sample in vector {
                let (_, value) = sample.value;
                if !value.is_nan() {
                    let name = extract_consumer_name(&sample.metric);
                    
                    consumers.push(ResourceConsumer {
                        labels: sample.metric.clone(),
                        name,
                        avg_usage: value,
                        max_usage: value,
                        min_usage: value,
                        percent_of_total: 0.0,
                    });
                    
                    total_sum += value;
                }
            }
        }
        _ => return Err("Query returned unexpected result type".into()),
    }
    
    // Sort by average usage (highest first)
    consumers.sort_by(|a, b| {
        b.avg_usage.partial_cmp(&a.avg_usage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    
    // Calculate percentages and take top N
    for consumer in &mut consumers {
        if total_sum > 0.0 {
            consumer.percent_of_total = (consumer.avg_usage / total_sum) * 100.0;
        }
    }
    
    consumers.truncate(top_n);
    
    Ok(ResourceUsageResult {
        query: query.to_string(),
        description: description.to_string(),
        top_consumers: consumers,
        total_usage: total_sum,
        time_range: (start, end),
    })
}

/// Extract a readable name from metric labels
fn extract_consumer_name(labels: &HashMap<String, String>) -> String {
    // Priority order for naming
    if let Some(name) = labels.get("name") {
        return name.clone();
    }
    
    if let Some(op) = labels.get("op") {
        return format!("{}", op);
    }
    
    if let Some(state) = labels.get("state") {
        return format!("{}", state);
    }

    if let Some(id) = labels.get("id") {
        return format!("id:{}", id);
    }
    
    // Show the metric name if no good labels
    if let Some(metric_name) = labels.get("__name__") {
        return metric_name.clone();
    }
    
    "unknown".to_string()
}

/// Format resource usage results for display
pub fn format_resource_usage(result: &ResourceUsageResult) -> String {
    let mut output = String::new();
    
    output.push_str(&format!(
        "Resource Usage Analysis: {}\n",
        result.description
    ));
    output.push_str(&format!("Query: {}\n", result.query));
    output.push_str("=" .repeat(60).as_str());
    output.push_str("\n\n");
    
    if result.top_consumers.is_empty() {
        output.push_str("No data found for this query.\n");
        return output;
    }
    
    output.push_str(&format!(
        "Top {} Consumers (Total: {:.2}):\n\n",
        result.top_consumers.len(),
        result.total_usage
    ));
    
    // Header
    output.push_str(&format!(
        "{:<60} {:>10} {:>10} {:>10} {:>8}\n",
        "Consumer", "Avg", "Max", "Min", "% Total"
    ));
    output.push_str(&format!(
        "{:<60} {:>10} {:>10} {:>10} {:>8}\n",
        "-".repeat(60), "-".repeat(10), "-".repeat(10), "-".repeat(10), "-".repeat(8)
    ));
    
    // Data rows
    for (i, consumer) in result.top_consumers.iter().enumerate() {
        output.push_str(&format!(
            "{:2}. {:<57} {:>10.2} {:>10.2} {:>10.2} {:>7.1}%\n",
            i + 1,
            truncate_string(&consumer.name, 57),
            consumer.avg_usage,
            consumer.max_usage,
            consumer.min_usage,
            consumer.percent_of_total
        ));
    }
    
    // Show cumulative percentage
    let cumulative_percent: f64 = result.top_consumers.iter()
        .map(|c| c.percent_of_total)
        .sum();
    
    output.push_str(&format!(
        "\nTop {} consumers account for {:.1}% of total usage\n",
        result.top_consumers.len(),
        cumulative_percent
    ));
    
    // Add interpretation
    if result.top_consumers.len() > 0 {
        let top = &result.top_consumers[0];
        if top.percent_of_total > 50.0 {
            output.push_str(&format!(
                "\nWARNING: {} dominates with {:.1}% of usage\n",
                top.name,
                top.percent_of_total
            ));
        } else if cumulative_percent < 50.0 && result.top_consumers.len() >= 5 {
            output.push_str("\nNOTE: Usage is distributed across many consumers\n");
        }
    }
    
    output
}

/// Truncate string to fit in column width
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Analyze multiple resource metrics
pub fn analyze_multiple_resources(
    engine: &Arc<QueryEngine>,
    tsdb: &Arc<Tsdb>,
) -> Result<Vec<ResourceUsageResult>, Box<dyn std::error::Error>> {
    let queries = vec![
        ("sum by(name) (irate(cgroup_cpu_usage[1m])) / 1e9", "Container CPU Usage"),
        ("sum by(name) (irate(cgroup_syscall[1m]))", "Container Syscall Rate"),
        ("sum by(id) (irate(cpu_usage[1m]))", "Per-Core CPU Usage"),
        ("sum by(op) (irate(syscall[1m]))", "System Call Types"),
    ];
    
    let mut results = Vec::new();
    
    for (query, description) in queries {
        match analyze_resource_usage(engine, tsdb, query, description, 10) {
            Ok(result) => results.push(result),
            Err(e) => eprintln!("Failed to analyze {}: {}", description, e),
        }
    }
    
    Ok(results)
}