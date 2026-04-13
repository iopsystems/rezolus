use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};
use redis::aio::ConnectionManager;
use reqwest::Url;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, error, info, warn};

/// Describes which Redis/Valkey INFO fields are monotonic counters.
/// Loaded from a JSON file via the `--counter-map` CLI flag.
///
/// Fields not listed here (and not matching the `total_` prefix heuristic)
/// are recorded as gauges.
///
/// ```json
/// {
///   "counters": ["expired_keys", "keyspace_hits"],
///   "float_counters": {
///     "used_cpu_sys": { "scale": 1000000, "unit": "microseconds" }
///   }
/// }
/// ```
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CounterMap {
    /// Metric field names that are monotonically increasing counters.
    #[serde(default)]
    counters: Vec<String>,
    /// Counters whose raw values are floats and need scaling before storage.
    /// Each entry maps a field name to its scale factor and unit label.
    #[serde(default)]
    float_counters: HashMap<String, FloatCounter>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FloatCounter {
    /// Multiply the raw float value by this factor before casting to u64.
    pub scale: f64,
    /// Unit label written into the metric metadata (e.g. "microseconds").
    pub unit: String,
}

impl CounterMap {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read counter map {}: {e}", path.display()))?;
        serde_json::from_str(&data)
            .map_err(|e| format!("failed to parse counter map {}: {e}", path.display()))
    }
}

/// Whitelist of metric field names to retain. When provided, any field not in
/// the set is discarded before conversion. Loaded from a JSON array via
/// `--filter`.
///
/// Keyspace sub-fields use the `keyspace/<sub_key>` form (e.g. `keyspace/keys`).
#[derive(Clone, Debug)]
pub struct MetricFilter {
    fields: HashSet<String>,
}

impl MetricFilter {
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read filter {}: {e}", path.display()))?;
        let list: Vec<String> = serde_json::from_str(&data)
            .map_err(|e| format!("failed to parse filter {}: {e}", path.display()))?;
        Ok(Self {
            fields: list.into_iter().collect(),
        })
    }
}

/// Holds the Redis/Valkey connection and converter for a recording session.
pub struct ValkeySource {
    connection: ConnectionManager,
    converter: ValkeyConverter,
    source_name: String,
    version: String,
}

/// Converts Redis/Valkey INFO output into SnapshotV2 objects.
/// Mirrors the PrometheusConverter pattern.
struct ValkeyConverter {
    metric_ids: HashMap<MetricKey, usize>,
    next_id: usize,
    known_counters: HashSet<String>,
    float_counters: HashMap<String, FloatCounter>,
    filter: Option<HashSet<String>>,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct MetricKey {
    name: String,
    labels: Vec<(String, String)>,
}

impl ValkeySource {
    pub async fn connect(
        url: &Url,
        interval: &humantime::Duration,
        counter_map: Option<CounterMap>,
        filter: Option<MetricFilter>,
    ) -> Result<Self, String> {
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

        // Detect whether this is Valkey or Redis and extract the version
        // by scanning the INFO output for server_name / *_version fields.
        let (source_name, version) = detect_service(&info_text);

        info!("connected to {source_name} {version} at {url}");

        Ok(Self {
            connection,
            converter: ValkeyConverter::new(counter_map.unwrap_or_default(), filter),
            source_name,
            version,
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

    pub fn descriptions(&self) -> &HashMap<String, String> {
        self.converter.descriptions()
    }
}

impl ValkeyConverter {
    fn new(map: CounterMap, filter: Option<MetricFilter>) -> Self {
        let mut known_counters: HashSet<String> = map.counters.into_iter().collect();
        // Float counters are also counters.
        known_counters.extend(map.float_counters.keys().cloned());
        Self {
            metric_ids: HashMap::new(),
            next_id: 0,
            known_counters,
            float_counters: map.float_counters,
            filter: filter.map(|f| f.fields),
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

    fn float_counter(&self, field: &str) -> Option<&FloatCounter> {
        self.float_counters.get(field)
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

            // Apply filter: discard fields not in the whitelist
            if let Some(ref allowed) = self.filter {
                if !allowed.contains(field) {
                    continue;
                }
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
                let mut metadata = Self::build_metadata(&metric_name, &labels);
                let counter_value = if let Some(fc) = self.float_counter(field) {
                    metadata.insert("unit".to_string(), fc.unit.clone());
                    (num * fc.scale) as u64
                } else {
                    num as u64
                };
                counters.push(Counter {
                    name: id,
                    value: counter_value,
                    metadata,
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

            // Filter uses "keyspace/<sub_key>" form
            if let Some(ref allowed) = self.filter {
                let filter_key = format!("keyspace/{sub_key}");
                if !allowed.contains(&filter_key) {
                    continue;
                }
            }

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

/// Detect whether the server is Valkey or Redis and return (source_name, version).
fn detect_service(info_text: &str) -> (String, String) {
    let mut is_valkey = false;
    let mut redis_version = String::new();
    let mut valkey_version = String::new();

    for line in info_text.lines() {
        let line = line.trim();
        if let Some((field, value)) = line.split_once(':') {
            match field.trim() {
                "server_name" if value.trim() == "valkey" => is_valkey = true,
                "valkey_version" => valkey_version = value.trim().to_string(),
                "redis_version" => redis_version = value.trim().to_string(),
                _ => {}
            }
        }
    }

    if is_valkey {
        let version = if valkey_version.is_empty() {
            redis_version
        } else {
            valkey_version
        };
        ("valkey".to_string(), version)
    } else {
        ("redis".to_string(), redis_version)
    }
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

    /// Build a CounterMap for testing.
    fn test_counter_map() -> CounterMap {
        CounterMap {
            counters: vec![
                "keyspace_hits".into(),
                "keyspace_misses".into(),
                "expired_keys".into(),
                "evicted_keys".into(),
            ],
            float_counters: HashMap::from([
                (
                    "used_cpu_sys".into(),
                    FloatCounter {
                        scale: 1_000_000.0,
                        unit: "microseconds".into(),
                    },
                ),
                (
                    "used_cpu_user".into(),
                    FloatCounter {
                        scale: 1_000_000.0,
                        unit: "microseconds".into(),
                    },
                ),
            ]),
        }
    }

    #[test]
    fn test_parse_simple_info() {
        let mut converter = ValkeyConverter::new(CounterMap::default(), None);
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
        let mut converter = ValkeyConverter::new(test_counter_map(), None);
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
        let mut converter = ValkeyConverter::new(CounterMap::default(), None);
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
        let mut converter = ValkeyConverter::new(test_counter_map(), None);
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
                assert_eq!(cpu_sys.metadata.get("unit").unwrap(), "microseconds");
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_stable_metric_ids() {
        let mut converter = ValkeyConverter::new(test_counter_map(), None);
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
        let mut converter = ValkeyConverter::new(CounterMap::default(), None);
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
        let mut converter = ValkeyConverter::new(CounterMap::default(), None);
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
    fn test_detect_service_valkey() {
        let info = "\
# Server\r
redis_version:7.2.4\r
server_name:valkey\r
valkey_version:8.0.1\r
os:Linux 5.15.0\r
";
        let (name, version) = detect_service(info);
        assert_eq!(name, "valkey");
        assert_eq!(version, "8.0.1");
    }

    #[test]
    fn test_detect_service_redis() {
        let info = "\
# Server\r
redis_version:7.2.4\r
redis_mode:standalone\r
";
        let (name, version) = detect_service(info);
        assert_eq!(name, "redis");
        assert_eq!(version, "7.2.4");
    }

    #[test]
    fn test_msgpack_roundtrip() {
        let mut converter = ValkeyConverter::new(test_counter_map(), None);
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
    fn test_filter_retains_only_listed_fields() {
        let filter = MetricFilter {
            fields: HashSet::from(["connected_clients".to_string(), "used_memory".to_string()]),
        };
        let mut converter = ValkeyConverter::new(CounterMap::default(), Some(filter));
        let info = "\
# Clients\r
connected_clients:10\r
blocked_clients:2\r
\r
# Memory\r
used_memory:1000000\r
used_memory_rss:2000000\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.gauges.len(), 2);
                let names: HashSet<_> = s
                    .gauges
                    .iter()
                    .map(|g| g.metadata.get("metric").unwrap().as_str())
                    .collect();
                assert!(names.contains("redis/connected_clients"));
                assert!(names.contains("redis/used_memory"));
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_filter_keyspace() {
        let filter = MetricFilter {
            fields: HashSet::from(["keyspace/keys".to_string()]),
        };
        let mut converter = ValkeyConverter::new(CounterMap::default(), Some(filter));
        let info = "\
# Keyspace\r
db0:keys=100,expires=10,avg_ttl=5000\r
";
        let snapshot = converter.convert(info);
        match snapshot {
            Snapshot::V2(s) => {
                assert_eq!(s.gauges.len(), 1);
                assert_eq!(
                    s.gauges[0].metadata.get("metric").unwrap(),
                    "redis/keyspace/keys"
                );
            }
            _ => panic!("expected V2 snapshot"),
        }
    }

    #[test]
    fn test_total_prefix_heuristic() {
        let mut converter = ValkeyConverter::new(CounterMap::default(), None);
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
