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
    pub source: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub protocol: Option<Protocol>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndpointStatus {
    Active,
    Pending,
}

/// Runtime state for a single endpoint during recording.
pub struct EndpointState {
    pub config: EndpointConfig,
    pub status: EndpointStatus,
    pub detected_protocol: Option<Protocol>,
    pub scrape_url: Option<Url>,
    pub systeminfo: Option<String>,
    pub descriptions: Option<String>,
    pub first_success_ns: Option<u64>,
    pub last_success_ns: Option<u64>,
}

impl EndpointState {
    pub fn new(config: EndpointConfig) -> Self {
        let detected_protocol = config.protocol.clone();
        Self {
            config,
            status: EndpointStatus::Pending,
            detected_protocol,
            scrape_url: None,
            systeminfo: None,
            descriptions: None,
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

/// Derive a source name from a URL when none is configured.
/// Logs a warning to stderr.
pub fn infer_source_name(url: &Url) -> String {
    let host = url.host_str().unwrap_or("unknown");
    let port = url.port().map(|p| format!("-{p}")).unwrap_or_default();
    let name = format!("{host}{port}");
    eprintln!("warn: no source name specified for {url}, using \"{name}\"");
    name
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
            source: "rezolus".to_string(),
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
            source: "rezolus".to_string(),
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
