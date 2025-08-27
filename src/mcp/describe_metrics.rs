use std::collections::HashMap;
use std::sync::Arc;
use crate::viewer::tsdb::Tsdb;

/// Get a hashmap of metric names to their descriptions by querying metriken metrics
fn get_metric_descriptions() -> HashMap<String, String> {
    let mut descriptions = HashMap::new();
    
    // Iterate through all registered metrics and extract their descriptions
    for metric in metriken::metrics().iter() {
        if let Some(description) = metric.description() {
            descriptions.insert(metric.name().to_string(), description.to_string());
        }
    }
    
    descriptions
}

/// Format metrics description for display
pub fn format_metrics_description(tsdb: &Arc<Tsdb>) -> String {
    let mut output = String::new();
    output.push_str("Available Metrics in Recording\n");
    output.push_str("===============================\n\n");

    // Get the descriptions from metriken metrics
    let descriptions = get_metric_descriptions();

    // List counters
    let mut counter_names = tsdb.counter_names();
    if !counter_names.is_empty() {
        counter_names.sort();
        output.push_str("COUNTERS (monotonically increasing values):\n");
        output.push_str("-------------------------------------------\n");
        for name in counter_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.counter_labels(name) {
                if !labels_list.is_empty() {
                    // Get unique label keys
                    let mut all_keys = std::collections::HashSet::new();
                    for labels in &labels_list {
                        for (key, _) in labels.inner.iter() {
                            all_keys.insert(key.clone());
                        }
                    }
                    if !all_keys.is_empty() {
                        let mut keys: Vec<_> = all_keys.into_iter().collect();
                        keys.sort();
                        output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                    }
                    output.push_str(&format!("  Series count: {}\n", labels_list.len()));
                }
            }
            output.push('\n');
        }
    }

    // List gauges
    let mut gauge_names = tsdb.gauge_names();
    if !gauge_names.is_empty() {
        gauge_names.sort();
        output.push_str("\nGAUGES (values that can go up or down):\n");
        output.push_str("----------------------------------------\n");
        for name in gauge_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.gauge_labels(name) {
                if !labels_list.is_empty() {
                    // Get unique label keys
                    let mut all_keys = std::collections::HashSet::new();
                    for labels in &labels_list {
                        for (key, _) in labels.inner.iter() {
                            all_keys.insert(key.clone());
                        }
                    }
                    if !all_keys.is_empty() {
                        let mut keys: Vec<_> = all_keys.into_iter().collect();
                        keys.sort();
                        output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                    }
                    output.push_str(&format!("  Series count: {}\n", labels_list.len()));
                }
            }
            output.push('\n');
        }
    }

    // List histograms
    let mut histogram_names = tsdb.histogram_names();
    if !histogram_names.is_empty() {
        histogram_names.sort();
        output.push_str("\nHISTOGRAMS (distributions of values):\n");
        output.push_str("--------------------------------------\n");
        for name in histogram_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.histogram_labels(name) {
                if !labels_list.is_empty() {
                    // Get unique label keys
                    let mut all_keys = std::collections::HashSet::new();
                    for labels in &labels_list {
                        for (key, _) in labels.inner.iter() {
                            all_keys.insert(key.clone());
                        }
                    }
                    if !all_keys.is_empty() {
                        let mut keys: Vec<_> = all_keys.into_iter().collect();
                        keys.sort();
                        output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                    }
                    output.push_str(&format!("  Series count: {}\n", labels_list.len()));
                }
            }
            output.push('\n');
        }
    }

    // Add summary statistics
    let total_counters = tsdb.counter_names().len();
    let total_gauges = tsdb.gauge_names().len();
    let total_histograms = tsdb.histogram_names().len();
    let total_metrics = total_counters + total_gauges + total_histograms;

    output.push_str("\nSUMMARY:\n");
    output.push_str("--------\n");
    output.push_str(&format!("Total unique metrics: {total_metrics}\n"));
    output.push_str(&format!("  Counters: {total_counters}\n"));
    output.push_str(&format!("  Gauges: {total_gauges}\n"));
    output.push_str(&format!("  Histograms: {total_histograms}\n"));
    output.push_str(&format!("\nSampling interval: {}ms\n", tsdb.interval() * 1000.0));

    output
}