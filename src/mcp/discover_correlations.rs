use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use std::collections::HashMap;
use std::sync::Arc;

use super::correlation::{calculate_correlation, CorrelationResult, SeriesCorrelation};

/// Parameters for correlation discovery
#[derive(Debug, Clone)]
pub struct DiscoveryParams {
    /// Minimum correlation threshold (default: 0.7)
    pub min_correlation: f64,
    /// Maximum time lag to test in seconds (default: 60)
    pub max_lag: i64,
    /// Maximum number of correlations to return per category (default: 10)
    pub max_results_per_category: usize,
}

impl Default for DiscoveryParams {
    fn default() -> Self {
        Self {
            min_correlation: 0.7,
            max_lag: 60,
            max_results_per_category: 10,
        }
    }
}

/// Represents a discovered correlation with metadata
#[derive(Debug, Clone)]
pub struct DiscoveredCorrelation {
    pub correlation_result: CorrelationResult,
    pub metric1_category: String,
    pub metric2_category: String,
    pub relationship_type: RelationshipType,
    pub query1_description: String,
    pub query2_description: String,
}

/// Types of cross-subsystem relationships
#[derive(Debug, Clone, PartialEq)]
pub enum RelationshipType {
    CrossSubsystem,  // Between different subsystems (most interesting)
    SameSubsystem,   // Within same subsystem
    Unknown,
}

/// Categorized correlation results
#[derive(Debug, Clone)]
pub struct CorrelationDiscoveryResult {
    pub params: DiscoveryParams,
    pub total_metrics_analyzed: usize,
    pub total_pairs_tested: usize,
    pub cross_subsystem_correlations: Vec<DiscoveredCorrelation>,
    pub same_subsystem_correlations: Vec<DiscoveredCorrelation>,
    pub execution_time_ms: u64,
}

/// Discover significant correlations automatically
pub fn discover_correlations(
    engine: &Arc<QueryEngine>,
    tsdb: &Arc<Tsdb>,
    params: Option<DiscoveryParams>,
) -> Result<CorrelationDiscoveryResult, Box<dyn std::error::Error>> {
    let start_time = std::time::Instant::now();
    let params = params.unwrap_or_default();
    
    // Get curated queries
    let queries = get_curated_queries();
    
    if queries.is_empty() {
        return Err("No curated queries available".into());
    }
    
    // Categorize queries by their category
    let categorized_queries = categorize_queries(&queries);
    
    // Generate query pairs, prioritizing cross-subsystem relationships
    let query_pairs = generate_query_pairs(&categorized_queries);
    
    let total_pairs = query_pairs.len();
    println!("Testing {} correlation pairs...", total_pairs);
    
    // Calculate correlations for all pairs
    let mut discovered_correlations = Vec::new();
    let mut processed = 0;
    
    for (query1, query2, cat1, cat2) in query_pairs {
        // Progress reporting every 50 pairs
        if processed % 50 == 0 {
            println!("Processed {}/{} pairs...", processed, total_pairs);
        }
        
        match calculate_correlation(engine, tsdb, &query1.query, &query2.query) {
            Ok(result) => {
                // Filter by correlation strength
                if result.max_correlation.abs() >= params.min_correlation {
                    let relationship_type = if cat1 == cat2 {
                        RelationshipType::SameSubsystem
                    } else {
                        RelationshipType::CrossSubsystem
                    };
                    
                    discovered_correlations.push(DiscoveredCorrelation {
                        correlation_result: result,
                        metric1_category: cat1,
                        metric2_category: cat2,
                        relationship_type,
                        query1_description: query1.description.clone(),
                        query2_description: query2.description.clone(),
                    });
                }
            }
            Err(e) => {
                // Skip failed correlations but log for debugging
                eprintln!("Correlation failed for {} vs {}: {}", query1.query, query2.query, e);
            }
        }
        
        processed += 1;
    }
    
    // Sort and categorize results
    let mut cross_subsystem = Vec::new();
    let mut same_subsystem = Vec::new();
    
    for corr in discovered_correlations {
        match corr.relationship_type {
            RelationshipType::CrossSubsystem => cross_subsystem.push(corr),
            RelationshipType::SameSubsystem => same_subsystem.push(corr),
            RelationshipType::Unknown => {}
        }
    }
    
    // Sort by absolute correlation strength
    cross_subsystem.sort_by(|a, b| {
        b.correlation_result.max_correlation.abs()
            .partial_cmp(&a.correlation_result.max_correlation.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    
    same_subsystem.sort_by(|a, b| {
        b.correlation_result.max_correlation.abs()
            .partial_cmp(&a.correlation_result.max_correlation.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    
    // Limit results
    cross_subsystem.truncate(params.max_results_per_category);
    same_subsystem.truncate(params.max_results_per_category);
    
    let execution_time = start_time.elapsed();
    
    Ok(CorrelationDiscoveryResult {
        params,
        total_metrics_analyzed: queries.len(),
        total_pairs_tested: total_pairs,
        cross_subsystem_correlations: cross_subsystem,
        same_subsystem_correlations: same_subsystem,
        execution_time_ms: execution_time.as_millis() as u64,
    })
}

/// Curated query with metadata
#[derive(Debug, Clone)]
struct CuratedQuery {
    query: String,
    category: String,
    description: String,
}

/// Get curated list of queries for correlation discovery
fn get_curated_queries() -> Vec<CuratedQuery> {
    vec![
        // BlockIO
        CuratedQuery {
            query: "sum(irate(blockio_operations[1m]))".to_string(),
            category: "blockio".to_string(),
            description: "BlockIO operations (IOPS)".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(blockio_bytes[1m]))".to_string(),
            category: "blockio".to_string(),
            description: "BlockIO throughput".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.5, blockio_latency)".to_string(),
            category: "blockio".to_string(),
            description: "BlockIO latency p50".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.99, blockio_latency)".to_string(),
            category: "blockio".to_string(),
            description: "BlockIO latency p99".to_string(),
        },


        // CPU
        CuratedQuery {
            query: "sum(irate(cpu_instructions[1m])) / sum(irate(cpu_cycles[1m]))".to_string(),
            category: "cpu".to_string(),
            description: "Average CPU Instructions per Cycle (IPC)".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(cpu_usage[1m])) / cpu_cores / 1e9".to_string(),
            category: "cpu".to_string(),
            description: "Average CPU usage percentage".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(cpu_usage{state=\"system\"}[1m])) / cpu_cores / 1e9".to_string(),
            category: "cpu".to_string(),
            description: "Average CPU system usage percentage".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(cpu_usage{state=\"user\"}[1m])) / cpu_cores / 1e9".to_string(),
            category: "cpu".to_string(),
            description: "Average CPU user usage percentage".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(cpu_migrations[1m]))".to_string(),
            category: "cpu".to_string(),
            description: "CPU migrations".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(cpu_tlb_flush[1m]))".to_string(),
            category: "cpu".to_string(),
            description: "CPU TLB flush".to_string(),
        },

        // Network
        CuratedQuery {
            query: "sum(irate(network_packets[1m]))".to_string(),
            category: "network".to_string(),
            description: "Network packet rate".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(network_bytes[1m]))".to_string(),
            category: "network".to_string(),
            description: "Network bandwidth".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.5, tcp_packet_latency)".to_string(),
            category: "network".to_string(),
            description: "TCP Packet latency p50".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.99, tcp_packet_latency)".to_string(),
            category: "network".to_string(),
            description: "TCP Packet latency p99".to_string(),
        },

        // Scheduler
        CuratedQuery {
            query: "histogram_quantile(0.5, scheduler_runqueue_latency)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler runqueue latency p50".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.99, scheduler_runqueue_latency)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler runqueue latency p99".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.5, scheduler_offcpu)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler off-cpu time p50".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.99, scheduler_offcpu)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler off-cpu time p99".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.5, scheduler_running)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler running time p50".to_string(),
        },
        CuratedQuery {
            query: "histogram_quantile(0.99, scheduler_running)".to_string(),
            category: "scheduler".to_string(),
            description: "Scheduler running time p99".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(scheduler_context_switch[1m]))".to_string(),
            category: "scheduler".to_string(),
            description: "Context switch rate".to_string(),
        },

        // Syscall
        CuratedQuery {
            query: "sum(irate(syscall[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Total syscall rate".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(syscall{op=\"read\"}[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Read syscall rate (read, recv, ...)".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(syscall{op=\"write\"}[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Write syscall rate (write, send, ...)".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(syscall{op=\"lock\"}[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Lock syscall rate (mutex)".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(syscall{op=\"yield\"}[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Yield syscall rate (yield)".to_string(),
        },

        // Softirq
        CuratedQuery {
            query: "sum(irate(softirq[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Rate of softirq".to_string(),
        },
        CuratedQuery {
            query: "sum(irate(softirq_time[1m]))".to_string(),
            category: "syscall".to_string(),
            description: "Time in softirq handlers".to_string(),
        },

        // cgroups
        CuratedQuery {
            query: "sum by(name) (irate(cgroup_cpu_usage[1m]))".to_string(),
            category: "cgroup".to_string(),
            description: "Per-cgroup CPU usage".to_string(),
        },
        CuratedQuery {
            query: "sum by(name) (irate(cgroup_cpu_usage{state=\"user\"}[1m]))".to_string(),
            category: "cgroup".to_string(),
            description: "Per-cgroup user CPU usage".to_string(),
        },
        CuratedQuery {
            query: "sum by(name) (irate(cgroup_cpu_usage{state=\"system\"}[1m]))".to_string(),
            category: "cgroup".to_string(),
            description: "Per-cgroup system CPU usage".to_string(),
        },
        CuratedQuery {
            query: "sum by(name) (irate(cgroup_cpu_migrations[1m]))".to_string(),
            category: "cgroup".to_string(),
            description: "Per-cgroup CPU migrations".to_string(),
        },
        CuratedQuery {
            query: "sum by(name) (irate(cgroup_cpu_instructions[1m])) / sum by(name) (irate(cgroup_cpu_cycles[1m]))".to_string(),
            category: "cgroup".to_string(),
            description: "Per-cgroup Instructions per Cycle (IPC)".to_string(),
        },
    ]
}

/// Categorize curated queries by their category
fn categorize_queries(queries: &[CuratedQuery]) -> HashMap<String, Vec<CuratedQuery>> {
    let mut categorized = HashMap::new();
    
    for query in queries {
        categorized
            .entry(query.category.clone())
            .or_insert_with(Vec::new)
            .push(query.clone());
    }
    
    categorized
}


/// Generate query pairs, prioritizing cross-subsystem relationships
fn generate_query_pairs(categorized_queries: &HashMap<String, Vec<CuratedQuery>>) -> Vec<(CuratedQuery, CuratedQuery, String, String)> {
    let mut pairs = Vec::new();
    
    // Get all category names
    let categories: Vec<String> = categorized_queries.keys().cloned().collect();
    
    // Generate cross-subsystem pairs first (these are most interesting)
    for (i, cat1) in categories.iter().enumerate() {
        for cat2 in categories.iter().skip(i + 1) {
            if let (Some(queries1), Some(queries2)) = 
                (categorized_queries.get(cat1), categorized_queries.get(cat2)) {
                
                // Test all combinations for curated queries (limited set)
                for query1 in queries1 {
                    for query2 in queries2 {
                        pairs.push((
                            query1.clone(), 
                            query2.clone(), 
                            cat1.clone(), 
                            cat2.clone()
                        ));
                    }
                }
            }
        }
    }
    
    // Add same-subsystem pairs for comparison
    for (category, queries) in categorized_queries {
        if queries.len() > 1 {
            for (i, query1) in queries.iter().enumerate() {
                for query2 in queries.iter().skip(i + 1) {
                    pairs.push((
                        query1.clone(), 
                        query2.clone(), 
                        category.clone(), 
                        category.clone()
                    ));
                }
            }
        }
    }
    
    pairs
}


/// Extract a readable label value from a metric's labels
fn extract_label_value(labels: &HashMap<String, String>) -> String {
    // Try to find the most descriptive label
    if let Some(name) = labels.get("name") {
        // For cgroups, extract just the service name
        if name.contains(".service") {
            return name.split('/').last()
                .unwrap_or(name)
                .replace(".service", "")
                .to_string();
        }
        return name.clone();
    }
    
    if let Some(id) = labels.get("id") {
        return id.clone();
    }
    
    if let Some(instance) = labels.get("instance") {
        return instance.clone();
    }
    
    // If no good label found, show all labels
    if !labels.is_empty() {
        let label_str: Vec<String> = labels.iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        return format!("{{{}}}", label_str.join(","));
    }
    
    "unlabeled".to_string()
}

/// Format discovery results for display
pub fn format_discovery_result(result: &CorrelationDiscoveryResult) -> String {
    let mut output = String::new();
    
    output.push_str("Automatic Correlation Discovery Results\n");
    output.push_str("=====================================\n\n");
    
    output.push_str(&format!(
        "Analysis Parameters:\n\
         - Minimum correlation threshold: {:.2}\n\
         - Maximum lag tested: {}s\n\
         - Metrics analyzed: {}\n\
         - Total pairs tested: {}\n\
         - Execution time: {}ms\n\n",
        result.params.min_correlation,
        result.params.max_lag,
        result.total_metrics_analyzed,
        result.total_pairs_tested,
        result.execution_time_ms
    ));
    
    // Cross-subsystem correlations (most interesting)
    if !result.cross_subsystem_correlations.is_empty() {
        output.push_str("CROSS-SUBSYSTEM CORRELATIONS (Most Interesting)\n");
        output.push_str("=============================================\n\n");
        
        for (i, discovered) in result.cross_subsystem_correlations.iter().enumerate() {
            let corr = &discovered.correlation_result;
            output.push_str(&format!(
                "{}. {} ↔ {} (r={:.3}, lag={}s)\n",
                i + 1,
                discovered.metric1_category.to_uppercase(),
                discovered.metric2_category.to_uppercase(),
                corr.max_correlation,
                corr.optimal_lag
            ));
            
            output.push_str(&format!(
                "   {} - {}\n",
                discovered.query1_description,
                corr.metric1
            ));
            output.push_str(&format!(
                "   {} - {}\n",
                discovered.query2_description,
                corr.metric2
            ));
            
            // Show top contributing series if this is a multi-series correlation
            if corr.series_pairs.len() > 1 {
                output.push_str(&format!("   Top contributing series ({} total pairs):\n", corr.series_pairs.len()));
                
                // Sort series pairs by absolute correlation
                let mut sorted_pairs = corr.series_pairs.clone();
                sorted_pairs.sort_by(|a, b| {
                    b.max_correlation.abs()
                        .partial_cmp(&a.max_correlation.abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                
                // Show top 3 contributors
                for (j, series_pair) in sorted_pairs.iter().take(3).enumerate() {
                    // Extract container/label name if present
                    let label1 = extract_label_value(&series_pair.labels1);
                    let label2 = extract_label_value(&series_pair.labels2);
                    
                    output.push_str(&format!(
                        "     {}. {} ↔ {} (r={:.3}, lag={}s)\n",
                        j + 1,
                        label1,
                        label2,
                        series_pair.max_correlation,
                        series_pair.optimal_lag
                    ));
                }
                
                if corr.series_pairs.len() > 3 {
                    output.push_str(&format!("     ... and {} more series pairs\n", corr.series_pairs.len() - 3));
                }
            }
            
            // Interpret the relationship
            if corr.optimal_lag > 0 {
                output.push_str(&format!(
                    "   → {} activity leads {} activity by {}s\n",
                    discovered.metric1_category,
                    discovered.metric2_category,
                    corr.optimal_lag
                ));
            } else if corr.optimal_lag < 0 {
                output.push_str(&format!(
                    "   → {} activity leads {} activity by {}s\n",
                    discovered.metric2_category,
                    discovered.metric1_category,
                    corr.optimal_lag.abs()
                ));
            } else {
                output.push_str(&format!(
                    "   → {} and {} activity are synchronous\n",
                    discovered.metric1_category,
                    discovered.metric2_category
                ));
            }
            
            output.push_str(&format!(
                "   Strength: {}\n\n",
                interpret_correlation_strength(corr.max_correlation)
            ));
        }
    } else {
        output.push_str("No significant cross-subsystem correlations found.\n\n");
    }
    
    // Same-subsystem correlations (for reference)
    if !result.same_subsystem_correlations.is_empty() {
        output.push_str("SAME-SUBSYSTEM CORRELATIONS (For Reference)\n");
        output.push_str("=========================================\n\n");
        
        for (i, discovered) in result.same_subsystem_correlations.iter().take(5).enumerate() {
            let corr = &discovered.correlation_result;
            output.push_str(&format!(
                "{}. Within {} (r={:.3}, lag={}s)\n",
                i + 1,
                discovered.metric1_category.to_uppercase(),
                corr.max_correlation,
                corr.optimal_lag
            ));
            
            output.push_str(&format!(
                "   {} vs {}\n",
                discovered.query1_description,
                discovered.query2_description
            ));
            output.push_str(&format!("   {} vs {}\n\n", corr.metric1, corr.metric2));
        }
        
        if result.same_subsystem_correlations.len() > 5 {
            output.push_str(&format!(
                "   ... and {} more same-subsystem correlations\n\n",
                result.same_subsystem_correlations.len() - 5
            ));
        }
    }
    
    output.push_str("INTERPRETATION GUIDE\n");
    output.push_str("==================\n");
    output.push_str("Cross-subsystem correlations are the most interesting findings as they reveal\n");
    output.push_str("how different parts of the system interact. For example:\n\n");
    output.push_str("• CPU → Memory: High CPU usage may lead to increased memory allocation\n");
    output.push_str("• Memory → Disk I/O: Memory pressure may trigger swapping or page faults\n");
    output.push_str("• Network → CPU: Network traffic processing consumes CPU cycles\n");
    output.push_str("• Disk I/O → CPU: Storage operations require CPU for processing\n\n");
    output.push_str("Positive correlations: metrics increase/decrease together\n");
    output.push_str("Negative correlations: one increases while the other decreases\n");
    output.push_str("Lag values: positive means metric 1 leads, negative means metric 2 leads\n");
    
    output
}

fn interpret_correlation_strength(r: f64) -> &'static str {
    let abs_r = r.abs();
    if abs_r >= 0.9 {
        if r > 0.0 { "Very strong positive" } else { "Very strong negative" }
    } else if abs_r >= 0.7 {
        if r > 0.0 { "Strong positive" } else { "Strong negative" }
    } else if abs_r >= 0.5 {
        if r > 0.0 { "Moderate positive" } else { "Moderate negative" }
    } else {
        "Weak"
    }
}