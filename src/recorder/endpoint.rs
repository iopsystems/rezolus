use reqwest::Url;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Msgpack,
    Prometheus,
}

fn deserialize_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Url::parse(&s).map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, Deserialize)]
pub struct EndpointConfig {
    #[serde(deserialize_with = "deserialize_url")]
    pub url: Url,
    /// None until probe resolves it: Msgpack → "rezolus",
    /// Prometheus → URL-derived (with warning).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub protocol: Option<Protocol>,
}

impl EndpointConfig {
    /// "?" only reachable on Pending endpoints (probe hasn't run).
    pub fn source_label(&self) -> &str {
        self.source.as_deref().unwrap_or("?")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndpointStatus {
    Active,
    Pending,
}

/// How an endpoint's data is obtained.
#[derive(Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// Scraped over HTTP (Rezolus msgpack or Prometheus text).
    Http,
    /// Read locally from the host's AMD GPU hardware counters (PMU). No HTTP
    /// scrape; the recorder samples the GPU directly each tick. Only constructed
    /// on Linux, where PMU recording is supported.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    AmdPmu,
}

/// Runtime state for a single endpoint during recording.
pub struct EndpointState {
    pub config: EndpointConfig,
    pub kind: EndpointKind,
    pub status: EndpointStatus,
    pub detected_protocol: Option<Protocol>,
    pub scrape_url: Option<Url>,
    pub systeminfo: Option<String>,
    pub descriptions: Option<String>,
    pub sampler_status: Option<String>,
    pub first_success_ns: Option<u64>,
    pub last_success_ns: Option<u64>,
}

impl EndpointState {
    pub fn new(config: EndpointConfig) -> Self {
        let detected_protocol = config.protocol.clone();
        Self {
            config,
            kind: EndpointKind::Http,
            status: EndpointStatus::Pending,
            detected_protocol,
            scrape_url: None,
            systeminfo: None,
            descriptions: None,
            sampler_status: None,
            first_success_ns: None,
            last_success_ns: None,
        }
    }

    /// A local AMD GPU PMU source. It is immediately `Active` (no probe), has no
    /// scrape URL, and is read directly from the GPU each tick. Only constructed
    /// on Linux, where PMU recording is supported.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn new_amd_pmu(source: String) -> Self {
        Self {
            config: EndpointConfig {
                url: Url::parse("pmu://amd/local").expect("static URL"),
                source: Some(source),
                role: None,
                protocol: None,
            },
            kind: EndpointKind::AmdPmu,
            status: EndpointStatus::Active,
            detected_protocol: None,
            scrape_url: None,
            systeminfo: None,
            descriptions: None,
            sampler_status: None,
            first_success_ns: None,
            last_success_ns: None,
        }
    }

    pub fn protocol(&self) -> Option<&Protocol> {
        self.detected_protocol
            .as_ref()
            .or(self.config.protocol.as_ref())
    }

    pub fn record_success(&mut self, timestamp_ns: u64) {
        if self.first_success_ns.is_none() {
            self.first_success_ns = Some(timestamp_ns);
        }
        self.last_success_ns = Some(timestamp_ns);
    }
}

pub fn infer_source_name(url: &Url) -> String {
    let host = url.host_str().unwrap_or("unknown");
    let port = url.port().map(|p| format!("-{p}")).unwrap_or_default();
    format!("{host}{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_source_name_with_port() {
        let url: Url = "http://localhost:4241/metrics".parse().unwrap();
        let name = infer_source_name(&url);
        assert_eq!(name, "localhost-4241");
    }

    #[test]
    fn test_infer_source_name_no_port() {
        let url: Url = "http://example.com/metrics".parse().unwrap();
        let name = infer_source_name(&url);
        assert_eq!(name, "example.com");
    }

    #[test]
    fn test_endpoint_state_new() {
        let config = EndpointConfig {
            url: "http://localhost:4241".parse().unwrap(),
            source: Some("rezolus".to_string()),
            role: None,
            protocol: Some(Protocol::Msgpack),
        };
        let state = EndpointState::new(config);
        assert_eq!(state.status, EndpointStatus::Pending);
        assert_eq!(state.protocol(), Some(&Protocol::Msgpack));
        assert!(state.first_success_ns.is_none());
    }

    #[test]
    fn test_record_success() {
        let config = EndpointConfig {
            url: "http://localhost:4241".parse().unwrap(),
            source: Some("rezolus".to_string()),
            role: None,
            protocol: None,
        };
        let mut state = EndpointState::new(config);
        state.record_success(1000);
        assert_eq!(state.first_success_ns, Some(1000));
        assert_eq!(state.last_success_ns, Some(1000));
        state.record_success(2000);
        assert_eq!(state.first_success_ns, Some(1000)); // unchanged
        assert_eq!(state.last_success_ns, Some(2000));
    }
}
