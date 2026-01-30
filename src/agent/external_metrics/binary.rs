use super::store::ExternalMetricsStore;
use super::types::{ConnectionContext, ExternalMetricValue};
use std::collections::HashMap;
use std::sync::Arc;

/// Magic bytes identifying the binary protocol: "REZL"
pub const MAGIC: [u8; 4] = [0x52, 0x45, 0x5A, 0x4C];

/// Current protocol version
pub const VERSION_MAJOR: u8 = 1;

#[allow(dead_code)]
pub const VERSION_MINOR: u8 = 0;

/// Maximum message size (64KB)
pub const MAX_MESSAGE_SIZE: usize = 65536;

/// Message type codes
const MSG_TYPE_SESSION: u8 = 0;
const MSG_TYPE_COUNTER: u8 = 1;
const MSG_TYPE_GAUGE: u8 = 2;
const MSG_TYPE_HISTOGRAM: u8 = 3;

/// Error types for binary protocol parsing
#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("invalid magic bytes")]
    InvalidMagic,
    #[error("unsupported version: {0}.{1}")]
    UnsupportedVersion(u8, u8),
    #[error("message too large: {0} bytes")]
    MessageTooLarge(usize),
    #[error("truncated header")]
    TruncatedHeader,
    #[error("truncated metric")]
    TruncatedMetric,
    #[error("invalid metric type: {0}")]
    InvalidMetricType(u8),
    #[error("invalid UTF-8 in metric name")]
    InvalidUtf8,
    #[error("invalid UTF-8 in label")]
    InvalidLabelUtf8,
    #[error("invalid histogram config")]
    InvalidHistogramConfig,
    #[error("per-connection metric limit exceeded")]
    ConnectionLimitExceeded,
}

/// Parse a binary protocol message and ingest metrics into the store.
/// Returns the number of metrics successfully ingested.
pub fn parse_and_ingest(
    data: &[u8],
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<usize, BinaryError> {
    // Header: magic[4] + version[2] + metric_count[2] + payload_size[4] = 12 bytes
    if data.len() < 12 {
        return Err(BinaryError::TruncatedHeader);
    }

    // Check magic
    if data[0..4] != MAGIC {
        return Err(BinaryError::InvalidMagic);
    }

    // Check version
    let version_major = data[4];
    let version_minor = data[5];
    if version_major != VERSION_MAJOR {
        return Err(BinaryError::UnsupportedVersion(
            version_major,
            version_minor,
        ));
    }

    let metric_count = u16::from_le_bytes([data[6], data[7]]) as usize;
    let payload_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    if payload_size > MAX_MESSAGE_SIZE {
        return Err(BinaryError::MessageTooLarge(payload_size));
    }

    if data.len() < 12 + payload_size {
        return Err(BinaryError::TruncatedMetric);
    }

    let payload = &data[12..12 + payload_size];
    parse_metrics(
        payload,
        metric_count,
        store,
        ctx,
        max_metrics_per_connection,
    )
}

fn parse_metrics(
    mut data: &[u8],
    metric_count: usize,
    store: &Arc<ExternalMetricsStore>,
    ctx: &mut ConnectionContext,
    max_metrics_per_connection: usize,
) -> Result<usize, BinaryError> {
    let mut ingested = 0;

    for _ in 0..metric_count {
        if data.is_empty() {
            break;
        }

        // Read message type
        let msg_type = data[0];
        data = &data[1..];

        // Handle session message (type 0) - just sets labels, no metric
        if msg_type == MSG_TYPE_SESSION {
            let (labels, remaining) = parse_labels(data)?;
            ctx.session_labels = labels;
            data = remaining;
            continue;
        }

        // Check per-connection limit before processing metric
        if ctx.metric_count >= max_metrics_per_connection {
            return Err(BinaryError::ConnectionLimitExceeded);
        }

        // Parse metric based on type
        let (value, remaining) = match msg_type {
            MSG_TYPE_COUNTER => parse_counter(data)?,
            MSG_TYPE_GAUGE => parse_gauge(data)?,
            MSG_TYPE_HISTOGRAM => parse_histogram(data)?,
            _ => return Err(BinaryError::InvalidMetricType(msg_type)),
        };
        data = remaining;

        // Read name length and name
        if data.len() < 2 {
            return Err(BinaryError::TruncatedMetric);
        }
        let name_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        data = &data[2..];

        if data.len() < name_len {
            return Err(BinaryError::TruncatedMetric);
        }
        let name = std::str::from_utf8(&data[..name_len])
            .map_err(|_| BinaryError::InvalidUtf8)?
            .to_string();
        data = &data[name_len..];

        // Read labels and merge with session labels
        let (mut labels, remaining) = parse_labels(data)?;
        data = remaining;

        // Merge session labels (metric-specific labels take precedence)
        for (k, v) in &ctx.session_labels {
            labels.entry(k.clone()).or_insert_with(|| v.clone());
        }

        // Ingest the metric
        if store.upsert(name, labels, value) {
            ctx.metric_count += 1;
            ingested += 1;
        }
    }

    Ok(ingested)
}

fn parse_counter(data: &[u8]) -> Result<(ExternalMetricValue, &[u8]), BinaryError> {
    if data.len() < 8 {
        return Err(BinaryError::TruncatedMetric);
    }
    let value = u64::from_le_bytes(data[..8].try_into().unwrap());
    Ok((ExternalMetricValue::Counter(value), &data[8..]))
}

fn parse_gauge(data: &[u8]) -> Result<(ExternalMetricValue, &[u8]), BinaryError> {
    if data.len() < 8 {
        return Err(BinaryError::TruncatedMetric);
    }
    let value = i64::from_le_bytes(data[..8].try_into().unwrap());
    Ok((ExternalMetricValue::Gauge(value), &data[8..]))
}

fn parse_histogram(data: &[u8]) -> Result<(ExternalMetricValue, &[u8]), BinaryError> {
    // Config: grouping_power[1] + max_value_power[1] + bucket_count[2] = 4 bytes
    if data.len() < 4 {
        return Err(BinaryError::TruncatedMetric);
    }

    let grouping_power = data[0];
    let max_value_power = data[1];
    let bucket_count = u16::from_le_bytes([data[2], data[3]]) as usize;

    // Validate histogram config
    if grouping_power >= max_value_power || max_value_power > 64 {
        return Err(BinaryError::InvalidHistogramConfig);
    }

    let data = &data[4..];

    // Read buckets
    let bucket_bytes = bucket_count * 8;
    if data.len() < bucket_bytes {
        return Err(BinaryError::TruncatedMetric);
    }

    let mut buckets = Vec::with_capacity(bucket_count);
    for i in 0..bucket_count {
        let offset = i * 8;
        let bucket = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        buckets.push(bucket);
    }

    Ok((
        ExternalMetricValue::Histogram {
            grouping_power,
            max_value_power,
            buckets,
        },
        &data[bucket_bytes..],
    ))
}

fn parse_labels(data: &[u8]) -> Result<(HashMap<String, String>, &[u8]), BinaryError> {
    if data.len() < 2 {
        return Err(BinaryError::TruncatedMetric);
    }

    let label_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    let mut data = &data[2..];
    let mut labels = HashMap::with_capacity(label_count);

    for _ in 0..label_count {
        // Read key
        if data.is_empty() {
            return Err(BinaryError::TruncatedMetric);
        }
        let key_len = data[0] as usize;
        data = &data[1..];

        if data.len() < key_len {
            return Err(BinaryError::TruncatedMetric);
        }
        let key = std::str::from_utf8(&data[..key_len])
            .map_err(|_| BinaryError::InvalidLabelUtf8)?
            .to_string();
        data = &data[key_len..];

        // Read value
        if data.is_empty() {
            return Err(BinaryError::TruncatedMetric);
        }
        let val_len = data[0] as usize;
        data = &data[1..];

        if data.len() < val_len {
            return Err(BinaryError::TruncatedMetric);
        }
        let val = std::str::from_utf8(&data[..val_len])
            .map_err(|_| BinaryError::InvalidLabelUtf8)?
            .to_string();
        data = &data[val_len..];

        labels.insert(key, val);
    }

    Ok((labels, data))
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
    fn test_parse_invalid_magic() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();
        let data = [0x00, 0x00, 0x00, 0x00, 1, 0, 0, 0, 0, 0, 0, 0];
        let result = parse_and_ingest(&data, &store, &mut ctx, 1000);
        assert!(matches!(result, Err(BinaryError::InvalidMagic)));
    }

    #[test]
    fn test_parse_unsupported_version() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();
        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC);
        data.extend_from_slice(&[2, 0]); // version 2.0
        data.extend_from_slice(&[0, 0]); // metric count
        data.extend_from_slice(&[0, 0, 0, 0]); // payload size

        let result = parse_and_ingest(&data, &store, &mut ctx, 1000);
        assert!(matches!(result, Err(BinaryError::UnsupportedVersion(2, 0))));
    }

    #[test]
    fn test_parse_empty_message() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();
        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC);
        data.extend_from_slice(&[VERSION_MAJOR, VERSION_MINOR]);
        data.extend_from_slice(&[0, 0]); // metric count = 0
        data.extend_from_slice(&[0, 0, 0, 0]); // payload size = 0

        let result = parse_and_ingest(&data, &store, &mut ctx, 1000);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_parse_counter() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        // Build message with one counter
        let mut payload = Vec::new();
        // Type: counter
        payload.push(MSG_TYPE_COUNTER);
        // Value: 42
        payload.extend_from_slice(&42u64.to_le_bytes());
        // Name: "test_counter"
        let name = b"test_counter";
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name);
        // Labels: empty
        payload.extend_from_slice(&0u16.to_le_bytes());

        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC);
        data.extend_from_slice(&[VERSION_MAJOR, VERSION_MINOR]);
        data.extend_from_slice(&1u16.to_le_bytes()); // metric count = 1
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);

        let result = parse_and_ingest(&data, &store, &mut ctx, 1000).unwrap();
        assert_eq!(result, 1);

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "test_counter");
        if let ExternalMetricValue::Counter(v) = active[0].value {
            assert_eq!(v, 42);
        } else {
            panic!("Expected counter");
        }
    }

    #[test]
    fn test_parse_gauge_with_labels() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        let mut payload = Vec::new();
        // Type: gauge
        payload.push(MSG_TYPE_GAUGE);
        // Value: -100
        payload.extend_from_slice(&(-100i64).to_le_bytes());
        // Name: "test_gauge"
        let name = b"test_gauge";
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name);
        // Labels: {"env": "prod"}
        payload.extend_from_slice(&1u16.to_le_bytes()); // 1 label
        payload.push(3); // key len
        payload.extend_from_slice(b"env");
        payload.push(4); // val len
        payload.extend_from_slice(b"prod");

        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC);
        data.extend_from_slice(&[VERSION_MAJOR, VERSION_MINOR]);
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);

        let result = parse_and_ingest(&data, &store, &mut ctx, 1000).unwrap();
        assert_eq!(result, 1);

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].labels.get("env"), Some(&"prod".to_string()));
        if let ExternalMetricValue::Gauge(v) = active[0].value {
            assert_eq!(v, -100);
        } else {
            panic!("Expected gauge");
        }
    }

    #[test]
    fn test_session_labels() {
        let store = make_store();
        let mut ctx = ConnectionContext::default();

        // Build message with session + counter
        let mut payload = Vec::new();

        // First: Session message (type 0) with labels
        payload.push(MSG_TYPE_SESSION);
        payload.extend_from_slice(&2u16.to_le_bytes()); // 2 labels
        payload.push(7); // key len
        payload.extend_from_slice(b"service");
        payload.push(5); // val len
        payload.extend_from_slice(b"myapp");
        payload.push(3); // key len
        payload.extend_from_slice(b"pid");
        payload.push(5); // val len
        payload.extend_from_slice(b"12345");

        // Second: Counter metric
        payload.push(MSG_TYPE_COUNTER);
        payload.extend_from_slice(&42u64.to_le_bytes());
        let name = b"requests";
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name);
        payload.extend_from_slice(&0u16.to_le_bytes()); // no additional labels

        let mut data = Vec::new();
        data.extend_from_slice(&MAGIC);
        data.extend_from_slice(&[VERSION_MAJOR, VERSION_MINOR]);
        data.extend_from_slice(&2u16.to_le_bytes()); // 2 messages
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&payload);

        let result = parse_and_ingest(&data, &store, &mut ctx, 1000).unwrap();
        assert_eq!(result, 1); // Only 1 metric ingested (session doesn't count)

        let active = store.get_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].labels.get("service"), Some(&"myapp".to_string()));
        assert_eq!(active[0].labels.get("pid"), Some(&"12345".to_string()));
    }
}
