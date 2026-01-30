use super::store::ExternalMetricsStore;
use super::types::{ConnectionContext, ExternalMetricValue};
use std::collections::HashMap;
use std::sync::Arc;

/// Error types for line protocol parsing
#[derive(Debug, thiserror::Error)]
pub enum LineError {
    #[error("empty metric name")]
    EmptyName,
    #[error("missing value")]
    MissingValue,
    #[error("invalid value type prefix")]
    InvalidTypePrefix,
    #[error("invalid counter value: {0}")]
    InvalidCounter(String),
    #[error("invalid gauge value: {0}")]
    InvalidGauge(String),
    #[error("invalid histogram format")]
    InvalidHistogram,
    #[error("unclosed labels")]
    UnclosedLabels,
    #[error("invalid label format")]
    InvalidLabelFormat,
    #[error("per-connection metric limit exceeded")]
    ConnectionLimitExceeded,
}

/// Result of parsing a line
pub enum ParseResult {
    /// Line was empty or a regular comment - no action taken
    Skipped,
    /// Session labels were set from a `# SESSION` directive
    SessionSet,
    /// Metric was successfully ingested
    MetricIngested,
    /// Metric was rejected (collision, limit, etc.)
    MetricRejected,
}

/// Parse a single line of the line protocol and ingest into the store.
/// Format: `metric_name{label="value"} type:value`
///
/// Special directives:
/// - `# SESSION key="value",key2="value2"` - Set session labels for this connection
///
/// Supported metric types:
/// - `counter:12345`
/// - `gauge:-42`
/// - `histogram:3,7:0 0 100 250 50`
pub fn parse_line_with_context(
    line: &str,
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<ParseResult, LineError> {
    let line = line.trim();

    // Skip empty lines
    if line.is_empty() {
        return Ok(ParseResult::Skipped);
    }

    // Check for session directive: # SESSION key="value",key2="value2"
    if let Some(rest) = line.strip_prefix("# SESSION ") {
        let labels = parse_labels(rest.trim())?;
        ctx.session_labels = labels;
        return Ok(ParseResult::SessionSet);
    }

    // Skip other comments
    if line.starts_with('#') {
        return Ok(ParseResult::Skipped);
    }

    // Check per-connection limit before parsing
    if ctx.metric_count >= max_metrics_per_connection {
        return Err(LineError::ConnectionLimitExceeded);
    }

    // Find the split between name+labels and value
    let (name_labels, value_str) = split_name_value(line)?;

    // Parse name and labels
    let (name, mut labels) = parse_name_labels(name_labels)?;

    // Merge session labels (metric-specific labels take precedence)
    for (k, v) in &ctx.session_labels {
        labels.entry(k.clone()).or_insert_with(|| v.clone());
    }

    // Parse value with type prefix
    let value = parse_value(value_str)?;

    // Ingest
    if store.upsert(name, labels, value) {
        ctx.metric_count += 1;
        Ok(ParseResult::MetricIngested)
    } else {
        Ok(ParseResult::MetricRejected)
    }
}

/// Parse a single line without connection context (for backwards compatibility in tests).
#[cfg(test)]
pub fn parse_line(line: &str, store: &Arc<ExternalMetricsStore>) -> Result<bool, LineError> {
    let mut ctx = ConnectionContext::default();
    match parse_line_with_context(line, store, &mut ctx, usize::MAX)? {
        ParseResult::MetricIngested => Ok(true),
        _ => Ok(false),
    }
}

fn split_name_value(line: &str) -> Result<(&str, &str), LineError> {
    // Find the FIRST space that separates name+labels from value
    // Handle the case where labels might contain spaces in quoted values
    let mut in_quotes = false;
    let mut in_braces = false;

    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '{' if !in_quotes => in_braces = true,
            '}' if !in_quotes => in_braces = false,
            ' ' if !in_quotes && !in_braces => {
                return Ok((&line[..i], line[i + 1..].trim()));
            }
            _ => {}
        }
    }

    Err(LineError::MissingValue)
}

fn parse_name_labels(s: &str) -> Result<(String, HashMap<String, String>), LineError> {
    // Check for labels
    if let Some(brace_start) = s.find('{') {
        let name = s[..brace_start].trim();
        if name.is_empty() {
            return Err(LineError::EmptyName);
        }

        let brace_end = s.rfind('}').ok_or(LineError::UnclosedLabels)?;
        if brace_end <= brace_start {
            return Err(LineError::UnclosedLabels);
        }

        let labels_str = &s[brace_start + 1..brace_end];
        let labels = parse_labels(labels_str)?;

        Ok((name.to_string(), labels))
    } else {
        // No labels
        let name = s.trim();
        if name.is_empty() {
            return Err(LineError::EmptyName);
        }
        Ok((name.to_string(), HashMap::new()))
    }
}

fn parse_labels(s: &str) -> Result<HashMap<String, String>, LineError> {
    let mut labels = HashMap::new();
    let s = s.trim();

    if s.is_empty() {
        return Ok(labels);
    }

    // Split by comma, but respect quoted values
    let mut current = String::new();
    let mut in_quotes = false;

    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                if !current.is_empty() {
                    let (k, v) = parse_single_label(&current)?;
                    labels.insert(k, v);
                    current.clear();
                }
            }
            _ => current.push(c),
        }
    }

    // Handle last label
    if !current.is_empty() {
        let (k, v) = parse_single_label(&current)?;
        labels.insert(k, v);
    }

    Ok(labels)
}

fn parse_single_label(s: &str) -> Result<(String, String), LineError> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() != 2 {
        return Err(LineError::InvalidLabelFormat);
    }

    let key = parts[0].trim().to_string();
    let mut value = parts[1].trim();

    // Remove surrounding quotes if present
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        value = &value[1..value.len() - 1];
    }

    if key.is_empty() {
        return Err(LineError::InvalidLabelFormat);
    }

    Ok((key, value.to_string()))
}

fn parse_value(s: &str) -> Result<ExternalMetricValue, LineError> {
    // Format: type:value
    let colon_pos = s.find(':').ok_or(LineError::InvalidTypePrefix)?;
    let type_prefix = &s[..colon_pos];
    let value_part = &s[colon_pos + 1..];

    match type_prefix {
        "counter" => {
            let v: u64 = value_part
                .trim()
                .parse()
                .map_err(|_| LineError::InvalidCounter(value_part.to_string()))?;
            Ok(ExternalMetricValue::Counter(v))
        }
        "gauge" => {
            let v: i64 = value_part
                .trim()
                .parse()
                .map_err(|_| LineError::InvalidGauge(value_part.to_string()))?;
            Ok(ExternalMetricValue::Gauge(v))
        }
        "histogram" => parse_histogram_value(value_part),
        _ => Err(LineError::InvalidTypePrefix),
    }
}

fn parse_histogram_value(s: &str) -> Result<ExternalMetricValue, LineError> {
    // Format: gp,mvp:bucket0 bucket1 bucket2 ...
    let colon_pos = s.find(':').ok_or(LineError::InvalidHistogram)?;
    let config_part = &s[..colon_pos];
    let buckets_part = &s[colon_pos + 1..];

    // Parse config
    let config_parts: Vec<&str> = config_part.split(',').collect();
    if config_parts.len() != 2 {
        return Err(LineError::InvalidHistogram);
    }

    let grouping_power: u8 = config_parts[0]
        .trim()
        .parse()
        .map_err(|_| LineError::InvalidHistogram)?;
    let max_value_power: u8 = config_parts[1]
        .trim()
        .parse()
        .map_err(|_| LineError::InvalidHistogram)?;

    // Validate config
    if grouping_power >= max_value_power || max_value_power > 64 {
        return Err(LineError::InvalidHistogram);
    }

    // Parse buckets
    let buckets: Result<Vec<u64>, _> = buckets_part
        .split_whitespace()
        .map(|s| s.parse::<u64>())
        .collect();
    let buckets = buckets.map_err(|_| LineError::InvalidHistogram)?;

    Ok(ExternalMetricValue::Histogram {
        grouping_power,
        max_value_power,
        buckets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Duration;

    fn make_store() -> Arc<ExternalMetricsStore> {
        Arc::new(ExternalMetricsStore::new(
            Duration::from_secs(60),
            1000,
            HashSet::new(),
        ))
    }

    #[test]
    fn test_parse_counter_no_labels() {
        let store = make_store();
        let result = parse_line("my_counter counter:42", &store);
        assert!(result.unwrap());

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "my_counter");
        assert!(active[0].labels.is_empty());
        if let ExternalMetricValue::Counter(v) = active[0].value {
            assert_eq!(v, 42);
        } else {
            panic!("Expected counter");
        }
    }

    #[test]
    fn test_parse_counter_with_labels() {
        let store = make_store();
        let result = parse_line(
            r#"my_counter{env="prod",region="us-east"} counter:100"#,
            &store,
        );
        assert!(result.unwrap());

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].labels.get("env"), Some(&"prod".to_string()));
        assert_eq!(active[0].labels.get("region"), Some(&"us-east".to_string()));
    }

    #[test]
    fn test_parse_gauge() {
        let store = make_store();
        let result = parse_line("temp{sensor=\"cpu\"} gauge:-5", &store);
        assert!(result.unwrap());

        let active = store.get_active();
        if let ExternalMetricValue::Gauge(v) = active[0].value {
            assert_eq!(v, -5);
        } else {
            panic!("Expected gauge");
        }
    }

    #[test]
    fn test_parse_histogram() {
        let store = make_store();
        let result = parse_line(
            "latency{service=\"api\"} histogram:3,7:0 0 100 250 50",
            &store,
        );
        assert!(result.unwrap());

        let active = store.get_active();
        if let ExternalMetricValue::Histogram {
            grouping_power,
            max_value_power,
            buckets,
        } = &active[0].value
        {
            assert_eq!(*grouping_power, 3);
            assert_eq!(*max_value_power, 7);
            assert_eq!(buckets, &vec![0, 0, 100, 250, 50]);
        } else {
            panic!("Expected histogram");
        }
    }

    #[test]
    fn test_skip_comments() {
        let store = make_store();
        let result = parse_line("# this is a comment", &store);
        assert!(!result.unwrap());
        assert!(store.is_empty());
    }

    #[test]
    fn test_skip_empty_lines() {
        let store = make_store();
        let result = parse_line("", &store);
        assert!(!result.unwrap());
        assert!(store.is_empty());
    }

    #[test]
    fn test_invalid_missing_value() {
        let store = make_store();
        let result = parse_line("my_counter", &store);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_type_prefix() {
        let store = make_store();
        let result = parse_line("my_metric unknown:42", &store);
        assert!(matches!(result, Err(LineError::InvalidTypePrefix)));
    }

    #[test]
    fn test_session_labels() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        // Set session labels
        let result = parse_line_with_context(
            r#"# SESSION service="myapp",pid="12345""#,
            &store,
            &mut ctx,
            1000,
        );
        assert!(matches!(result, Ok(ParseResult::SessionSet)));
        assert_eq!(
            ctx.session_labels.get("service"),
            Some(&"myapp".to_string())
        );
        assert_eq!(ctx.session_labels.get("pid"), Some(&"12345".to_string()));

        // Now send a metric - session labels should be merged
        let result = parse_line_with_context("my_counter counter:42", &store, &mut ctx, 1000);
        assert!(matches!(result, Ok(ParseResult::MetricIngested)));

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].labels.get("service"), Some(&"myapp".to_string()));
        assert_eq!(active[0].labels.get("pid"), Some(&"12345".to_string()));
    }

    #[test]
    fn test_session_labels_override() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        // Set session labels
        parse_line_with_context(r#"# SESSION env="default""#, &store, &mut ctx, 1000).unwrap();

        // Metric-specific label should override session label
        let result = parse_line_with_context(
            r#"my_counter{env="override"} counter:42"#,
            &store,
            &mut ctx,
            1000,
        );
        assert!(matches!(result, Ok(ParseResult::MetricIngested)));

        let active = store.get_active();
        assert_eq!(active[0].labels.get("env"), Some(&"override".to_string()));
    }

    #[test]
    fn test_per_connection_limit() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        // Set a limit of 2 metrics per connection
        let max_per_conn = 2;

        assert!(matches!(
            parse_line_with_context("m1 counter:1", &store, &mut ctx, max_per_conn),
            Ok(ParseResult::MetricIngested)
        ));
        assert!(matches!(
            parse_line_with_context("m2 counter:2", &store, &mut ctx, max_per_conn),
            Ok(ParseResult::MetricIngested)
        ));

        // Third metric should fail
        assert!(matches!(
            parse_line_with_context("m3 counter:3", &store, &mut ctx, max_per_conn),
            Err(LineError::ConnectionLimitExceeded)
        ));

        assert_eq!(ctx.metric_count, 2);
    }
}
