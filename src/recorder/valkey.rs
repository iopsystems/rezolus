use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};
use redis::aio::ConnectionManager;
use reqwest::Url;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, error, info, warn};

/// Known counter metrics in Redis/Valkey INFO output.
/// These are monotonically increasing values.
static KNOWN_COUNTERS: &[&str] = &[
    // Stats
    "total_connections_received",
    "total_commands_processed",
    "total_net_input_bytes",
    "total_net_output_bytes",
    "total_net_repl_input_bytes",
    "total_net_repl_output_bytes",
    "rejected_connections",
    "sync_full",
    "sync_partial_ok",
    "sync_partial_err",
    "expired_keys",
    "expired_time_cap_reached_count",
    "evicted_keys",
    "evicted_clients",
    "evicted_scripts",
    "keyspace_hits",
    "keyspace_misses",
    "total_forks",
    "total_reads_processed",
    "total_writes_processed",
    "io_threaded_reads_processed",
    "io_threaded_writes_processed",
    "reply_buffer_shrinks",
    "reply_buffer_expands",
    "total_error_replies",
    "dump_payload_sanitizations",
    "total_active_defrag_time",
    "total_eviction_exceeded_time",
    "eventloop_cycles",
    "eventloop_duration_sum",
    "eventloop_duration_cmd_sum",
    "expire_cycle_cpu_milliseconds",
    "acl_access_denied_auth",
    "acl_access_denied_cmd",
    "acl_access_denied_key",
    "acl_access_denied_channel",
    "client_query_buffer_limit_disconnections",
    "client_output_buffer_limit_disconnections",
    // Persistence
    "rdb_changes_since_last_save",
    "rdb_saves",
    "aof_rewrites",
    "aof_delayed_fsync",
    // CPU (float values, scaled to microseconds)
    "used_cpu_sys",
    "used_cpu_user",
    "used_cpu_sys_children",
    "used_cpu_user_children",
    "used_cpu_sys_main_thread",
    "used_cpu_user_main_thread",
];

/// Float counter metrics that need scaling (seconds -> microseconds).
static FLOAT_COUNTERS: &[&str] = &[
    "used_cpu_sys",
    "used_cpu_user",
    "used_cpu_sys_children",
    "used_cpu_user_children",
    "used_cpu_sys_main_thread",
    "used_cpu_user_main_thread",
];

/// Service version fields to capture for parquet metadata.
static SERVICE_INFO_FIELDS: &[&str] = &["redis_version", "valkey_version", "server_name"];

/// Holds the Redis/Valkey connection and converter for a recording session.
pub struct ValkeySource {
    connection: ConnectionManager,
    converter: ValkeyConverter,
    source_name: String,
    version: String,
    systeminfo_json: Option<String>,
}

/// Converts Redis/Valkey INFO output into SnapshotV2 objects.
/// Mirrors the PrometheusConverter pattern.
struct ValkeyConverter {
    metric_ids: HashMap<MetricKey, usize>,
    next_id: usize,
    known_counters: HashSet<&'static str>,
    float_counters: HashSet<&'static str>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct MetricKey {
    name: String,
    labels: Vec<(String, String)>,
}

impl ValkeySource {
    pub async fn connect(url: &Url, interval: &humantime::Duration) -> Result<Self, String> {
        let redis_url = normalize_url(url);

        let client = redis::Client::open(redis_url.as_str())
            .map_err(|e| format!("failed to create Redis client: {e}"))?;

        let mut connection = ConnectionManager::new(client)
            .await
            .map_err(|e| format!("failed to connect to Redis/Valkey: {e}"))?;

        // Verify connectivity with a probe INFO call + latency check
        let start = Instant::now();
        let info_text: String = redis::cmd("INFO")
            .query_async(&mut connection)
            .await
            .map_err(|e| format!("failed to run INFO command: {e}"))?;
        let latency = start.elapsed();

        let interval_dur: Duration = (*interval).into();
        if latency.as_nanos() >= interval_dur.as_nanos() {
            let recommended = humantime::Duration::from(Duration::from_millis(
                (latency * 2).as_nanos().div_ceil(1_000_000) as u64,
            ));
            return Err(format!(
                "sampling latency ({} us) exceeded the sample interval. \
                 Try setting the interval to: {recommended}",
                latency.as_micros()
            ));
        } else if latency.as_nanos() >= (3 * interval_dur.as_nanos() / 4) {
            warn!(
                "sampling latency ({} us) is more that 75% of the sample interval. \
                 Consider increasing the interval",
                latency.as_micros()
            );
        } else {
            debug!("sampling latency: {} us", latency.as_micros());
        }

        // Extract server metadata from the probe INFO response
        let server_info = parse_server_info(&info_text);
        let source_name = if server_info.get("server_name").map(|s| s.as_str()) == Some("valkey") {
            "valkey".to_string()
        } else {
            "redis".to_string()
        };
        let version = server_info
            .get("valkey_version")
            .or_else(|| server_info.get("redis_version"))
            .cloned()
            .unwrap_or_default();
        let systeminfo_json = serde_json::to_string(&server_info).ok();

        info!("connected to {source_name} {version} at {url}");

        Ok(Self {
            connection,
            converter: ValkeyConverter::new(),
            source_name,
            version,
            systeminfo_json,
        })
    }

    /// Fetch INFO, convert to Snapshot, serialize to msgpack bytes.
    pub async fn fetch_snapshot(&mut self) -> Option<Vec<u8>> {
        let info: String = match redis::cmd("INFO").query_async(&mut self.connection).await {
            Ok(s) => s,
            Err(e) => {
                error!("failed to run INFO command: {e}");
                return None;
            }
        };

        let snapshot = self.converter.convert(&info);
        match rmp_serde::encode::to_vec(&snapshot) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                error!("error serializing snapshot: {e}");
                None
            }
        }
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    pub fn server_info_json(&self) -> Option<&str> {
        self.systeminfo_json.as_deref()
    }

    pub fn descriptions(&self) -> &HashMap<String, String> {
        self.converter.descriptions()
    }
}

impl ValkeyConverter {
    fn new() -> Self {
        Self {
            metric_ids: HashMap::new(),
            next_id: 0,
            known_counters: KNOWN_COUNTERS.iter().copied().collect(),
            float_counters: FLOAT_COUNTERS.iter().copied().collect(),
        }
    }

    fn descriptions(&self) -> &HashMap<String, String> {
        static DESCRIPTIONS: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(|| {
                let mut d = HashMap::new();
                d.insert(
                    "redis/connected_clients".into(),
                    "Number of client connections (excluding replicas)".into(),
                );
                d.insert(
                    "redis/blocked_clients".into(),
                    "Clients pending on a blocking call".into(),
                );
                d.insert(
                    "redis/used_memory".into(),
                    "Total bytes allocated by the allocator".into(),
                );
                d.insert(
                    "redis/used_memory_rss".into(),
                    "Resident set size in bytes (OS-reported)".into(),
                );
                d.insert(
                    "redis/used_memory_peak".into(),
                    "Peak memory consumed in bytes".into(),
                );
                d.insert(
                    "redis/total_connections_received".into(),
                    "Total connections accepted by the server".into(),
                );
                d.insert(
                    "redis/total_commands_processed".into(),
                    "Total number of commands processed".into(),
                );
                d.insert(
                    "redis/instantaneous_ops_per_sec".into(),
                    "Number of commands processed per second".into(),
                );
                d.insert(
                    "redis/keyspace_hits".into(),
                    "Successful key lookups in the main dictionary".into(),
                );
                d.insert(
                    "redis/keyspace_misses".into(),
                    "Failed key lookups in the main dictionary".into(),
                );
                d.insert(
                    "redis/used_cpu_sys".into(),
                    "System CPU consumed by the server (microseconds, cumulative)".into(),
                );
                d.insert(
                    "redis/used_cpu_user".into(),
                    "User CPU consumed by the server (microseconds, cumulative)".into(),
                );
                d.insert(
                    "redis/expired_keys".into(),
                    "Total key expiration events".into(),
                );
                d.insert(
                    "redis/evicted_keys".into(),
                    "Keys evicted due to maxmemory limit".into(),
                );
                d.insert(
                    "redis/total_net_input_bytes".into(),
                    "Total bytes read from the network".into(),
                );
                d.insert(
                    "redis/total_net_output_bytes".into(),
                    "Total bytes written to the network".into(),
                );
                d
            });
        &DESCRIPTIONS
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

    fn build_metadata(name: &str, labels: &[(String, String)]) -> HashMap<String, String> {
        let mut metadata = HashMap::new();
        metadata.insert("metric".to_string(), name.to_string());
        for (k, v) in labels {
            metadata.insert(k.clone(), v.clone());
        }
        metadata
    }

    fn is_counter(&self, field: &str) -> bool {
        self.known_counters.contains(field) || field.starts_with("total_")
    }

    pub fn convert(&mut self, info_text: &str) -> Snapshot {
        let mut counters = Vec::new();
        let mut gauges = Vec::new();
        let mut current_section = String::new();

        for line in info_text.lines() {
            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            // Section header
            if let Some(section) = line.strip_prefix("# ") {
                current_section = section.trim().to_lowercase();
                continue;
            }

            let Some((field, value)) = line.split_once(':') else {
                continue;
            };

            let field = field.trim();
            let value = value.trim();

            // Handle Keyspace entries: db0:keys=100,expires=10,avg_ttl=5000
            if current_section == "keyspace" {
                self.parse_keyspace_entry(field, value, &mut gauges);
                continue;
            }

            // Skip human-readable duplicate fields
            if field.ends_with("_human") {
                continue;
            }

            // Try parsing as f64
            let Ok(num) = value.parse::<f64>() else {
                continue;
            };

            if !num.is_finite() {
                continue;
            }

            let metric_name = format!("redis/{field}");
            let labels = vec![("section".to_string(), current_section.clone())];

            if self.is_counter(field) {
                let id = self.get_or_assign_id(&metric_name, &labels);
                let counter_value = if self.float_counters.contains(field) {
                    // Convert seconds to microseconds for precision
                    (num * 1_000_000.0) as u64
                } else {
                    num as u64
                };
                counters.push(Counter {
                    name: id,
                    value: counter_value,
                    metadata: Self::build_metadata(&metric_name, &labels),
                });
            } else {
                let id = self.get_or_assign_id(&metric_name, &labels);
                gauges.push(Gauge {
                    name: id,
                    value: num as i64,
                    metadata: Self::build_metadata(&metric_name, &labels),
                });
            }
        }

        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::now(),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges,
            histograms: Vec::new(),
        })
    }

    fn parse_keyspace_entry(&mut self, db_name: &str, value: &str, gauges: &mut Vec<Gauge>) {
        let labels = vec![("db".to_string(), db_name.to_string())];

        for kv in value.split(',') {
            let Some((sub_key, sub_value)) = kv.split_once('=') else {
                continue;
            };
            let Ok(num) = sub_value.trim().parse::<i64>() else {
                continue;
            };

            let metric_name = format!("redis/keyspace/{sub_key}");
            let id = self.get_or_assign_id(&metric_name, &labels);
            gauges.push(Gauge {
                name: id,
                value: num,
                metadata: Self::build_metadata(&metric_name, &labels),
            });
        }
    }
}

/// Parse the Server section of an INFO response to extract metadata strings.
fn parse_server_info(info_text: &str) -> HashMap<String, String> {
    let fields: HashSet<&str> = SERVICE_INFO_FIELDS.iter().copied().collect();
    let mut result = HashMap::new();
    let mut in_server_section = false;

    for line in info_text.lines() {
        let line = line.trim();
        if line.starts_with("# ") {
            in_server_section = line.eq_ignore_ascii_case("# Server");
            if !in_server_section && !result.is_empty() {
                // Past the Server section, stop scanning
                break;
            }
            continue;
        }
        if !in_server_section {
            continue;
        }
        if let Some((field, value)) = line.split_once(':') {
            let field = field.trim();
            if fields.contains(field) {
                result.insert(field.to_string(), value.trim().to_string());
            }
        }
    }

    result
}

/// Normalize valkey:// to redis:// and valkeys:// to rediss:// for the redis crate.
fn normalize_url(url: &Url) -> String {
    let s = url.as_str();
    if let Some(rest) = s.strip_prefix("valkeys://") {
        format!("rediss://{rest}")
    } else if let Some(rest) = s.strip_prefix("valkey://") {
        format!("redis://{rest}")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_info() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Server\r
redis_version:7.2.4\r
uptime_in_seconds:12345\r
\r
# Clients\r
connected_clients:10\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                // uptime_in_seconds and connected_clients are gauges
                assert!(s.gauges.iter().any(|g| g
                    .metadata
                    .get("metric")
                    .map(|m| m == "redis/uptime_in_seconds")
                    .unwrap_or(false)));
                assert!(s.gauges.iter().any(|g| g
                    .metadata
                    .get("metric")
                    .map(|m| m == "redis/connected_clients")
                    .unwrap_or(false)
                    && g.value == 10));
                // redis_version is non-numeric, should be skipped
                assert!(s.gauges.iter().all(|g| g
                    .metadata
                    .get("metric")
                    .map(|m| m != "redis/redis_version")
                    .unwrap_or(true)));
                assert!(s.counters.is_empty());
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_counter_classification() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Stats\r
total_connections_received:100\r
connected_clients:5\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.counters.len(), 1);
                assert_eq!(s.gauges.len(), 1);
                assert_eq!(s.counters[0].value, 100);
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_keyspace_parsing() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Keyspace\r
db0:keys=100,expires=10,avg_ttl=5000\r
db1:keys=50,expires=5,avg_ttl=3000\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.gauges.len(), 6); // 3 per db, 2 dbs
                assert!(s.gauges.iter().any(|g| g
                    .metadata
                    .get("metric")
                    .map(|m| m == "redis/keyspace/keys")
                    .unwrap_or(false)
                    && g.metadata.get("db").map(|d| d == "db0").unwrap_or(false)
                    && g.value == 100));
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_float_cpu_scaling() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# CPU\r
used_cpu_sys:10.500000\r
used_cpu_user:20.300000\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.counters.len(), 2);
                let cpu_sys = s
                    .counters
                    .iter()
                    .find(|c| {
                        c.metadata
                            .get("metric")
                            .map(|m| m == "redis/used_cpu_sys")
                            .unwrap_or(false)
                    })
                    .unwrap();
                assert_eq!(cpu_sys.value, 10_500_000); // 10.5 * 1_000_000
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_stable_metric_ids() {
        let mut converter = ValkeyConverter::new();
        let info1 = "\
# Stats\r
total_connections_received:100\r
connected_clients:5\r
";
        let info2 = "\
# Stats\r
total_connections_received:200\r
connected_clients:10\r
";

        let s1 = converter.convert(info1);
        let s2 = converter.convert(info2);

        match (s1, s2) {
            (Snapshot::V2(snap1), Snapshot::V2(snap2)) => {
                let ids1: Vec<_> = snap1.counters.iter().map(|c| &c.name).collect();
                let ids2: Vec<_> = snap2.counters.iter().map(|c| &c.name).collect();
                assert_eq!(ids1, ids2);

                let gids1: Vec<_> = snap1.gauges.iter().map(|g| &g.name).collect();
                let gids2: Vec<_> = snap2.gauges.iter().map(|g| &g.name).collect();
                assert_eq!(gids1, gids2);
            }
            _ => panic!("expected V2 snapshots"),
        }
    }

    #[test]
    fn test_non_numeric_values_skipped() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Server\r
redis_version:7.2.4\r
redis_mode:standalone\r
os:Linux 5.15.0\r
uptime_in_seconds:100\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                // Only uptime_in_seconds is numeric
                assert_eq!(s.counters.len() + s.gauges.len(), 1);
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_human_readable_fields_skipped() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Memory\r
used_memory:1000000\r
used_memory_human:976.56K\r
used_memory_rss:2000000\r
used_memory_rss_human:1.91M\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.gauges.len(), 2);
                assert!(s.gauges.iter().all(|g| !g
                    .metadata
                    .get("metric")
                    .map(|m| m.ends_with("_human"))
                    .unwrap_or(false)));
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_normalize_url() {
        let url = normalize_url(&Url::parse("redis://localhost:6379").unwrap());
        assert!(url.starts_with("redis://localhost:6379"));

        let url = normalize_url(&Url::parse("valkey://localhost:6379").unwrap());
        assert!(url.starts_with("redis://localhost:6379"));
        assert!(!url.starts_with("valkey://"));

        let url = normalize_url(&Url::parse("valkeys://localhost:6379").unwrap());
        assert!(url.starts_with("rediss://localhost:6379"));

        let url = normalize_url(&Url::parse("valkey://:pass@host:6379").unwrap());
        assert!(url.starts_with("redis://:pass@host:6379"));
    }

    #[test]
    fn test_parse_server_info() {
        let info = "\
# Server\r
redis_version:7.2.4\r
server_name:valkey\r
valkey_version:8.0.1\r
os:Linux 5.15.0-1024-aws x86_64\r
arch_bits:64\r
tcp_port:6379\r
redis_mode:standalone\r
run_id:abc123def456\r
\r
# Clients\r
connected_clients:10\r
";
        let info_map = parse_server_info(info);
        assert_eq!(info_map.get("redis_version").unwrap(), "7.2.4");
        assert_eq!(info_map.get("server_name").unwrap(), "valkey");
        assert_eq!(info_map.get("valkey_version").unwrap(), "8.0.1");
        // Only service version fields are captured
        assert!(!info_map.contains_key("os"));
        assert!(!info_map.contains_key("arch_bits"));
        assert!(!info_map.contains_key("connected_clients"));
    }

    #[test]
    fn test_msgpack_roundtrip() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Stats\r
total_connections_received:100\r
connected_clients:5\r
";
        let snapshot = converter.convert(info);
        let bytes = rmp_serde::encode::to_vec(&snapshot).unwrap();
        let decoded: Snapshot = rmp_serde::from_slice(&bytes).unwrap();
        match decoded {
            Snapshot::V2(s) => {
                assert_eq!(s.counters.len(), 1);
                assert_eq!(s.gauges.len(), 1);
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_total_prefix_heuristic() {
        let mut converter = ValkeyConverter::new();
        let info = "\
# Stats\r
total_some_new_metric:42\r
some_gauge_metric:10\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.counters.len(), 1);
                assert_eq!(s.gauges.len(), 1);
                assert_eq!(
                    s.counters[0].metadata.get("metric").unwrap(),
                    "redis/total_some_new_metric"
                );
            }
            _ => panic!("expected V2 snapshot"),
        }
    }
}
