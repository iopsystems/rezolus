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

/// Runtime state for a single endpoint during recording.
pub struct EndpointState {
    pub config: EndpointConfig,
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
    match distinguishing_path(url) {
        Some(path) => format!("{host}{port}-{path}"),
        None => format!("{host}{port}"),
    }
}

/// The part of a URL's path worth putting in a source name, or `None` when it
/// carries nothing a host and port do not already say.
///
/// **Host and port alone are not unique per target.** Several exporters behind
/// one address, distinguished only by path, is an ordinary Prometheus
/// deployment — `/metrics` and `/federate`, or a path per tenant — and it is
/// far more common there than for a Rezolus agent, which is one per host on a
/// fixed port. Two targets inferring the SAME source get identical label sets,
/// and then nothing downstream can tell their recordings apart: no `--recording`
/// selector names either, and the viewer falls back to positional aliases. The
/// recorder warns about that, but a warning is a poor substitute for a name.
///
/// `/metrics` is excluded because it is the convention rather than a
/// distinction: appending it to every inferred name would be noise on the
/// common case and would rename every existing capture's source for nothing.
fn distinguishing_path(url: &Url) -> Option<String> {
    let path = url.path().trim_matches('/');
    if path.is_empty() || path == "metrics" {
        return None;
    }
    // Slashes and anything else awkward become `-`: this ends up as a label
    // value, a `.rez` recording's directory slug and a `--separate` filename,
    // so it has to be a plain word in all three.
    let slug: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse the runs the mapping above can create (`/metrics//v2/` →
    // `metrics--v2`) and trim the edges.
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!slug.is_empty()).then_some(slug)
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

    /// Several exporters behind one address, distinguished only by path, is an
    /// ordinary Prometheus deployment. Inferring the same source for both gives
    /// them identical label sets, and then nothing downstream can tell their
    /// recordings apart: no `--recording` selector names either, and the viewer
    /// falls back to positional aliases. The recorder warns, but a warning is a
    /// poor substitute for a name.
    #[test]
    fn two_targets_on_one_address_infer_different_sources() {
        let a: Url = "http://svc:9090/metrics".parse().unwrap();
        let b: Url = "http://svc:9090/federate".parse().unwrap();
        assert_ne!(infer_source_name(&a), infer_source_name(&b));
    }

    /// `/metrics` is the convention, not a distinction: appending it to every
    /// inferred name would be noise on the common case and would rename every
    /// existing capture's source for nothing.
    #[test]
    fn the_conventional_metrics_path_is_not_part_of_the_name() {
        for url in [
            "http://svc:9090/metrics",
            "http://svc:9090/metrics/",
            "http://svc:9090/",
            "http://svc:9090",
        ] {
            let url: Url = url.parse().unwrap();
            assert_eq!(infer_source_name(&url), "svc-9090", "{url}");
        }
    }

    /// The inferred name becomes a label value, a `.rez` recording's directory
    /// slug and a `--separate` filename, so it has to be a plain word in all
    /// three — no slashes, no runs of separators, no leading or trailing dash.
    #[test]
    fn a_nested_path_becomes_one_plain_word() {
        let url: Url = "http://svc:9090/metrics//v2/tenant_a/".parse().unwrap();
        let name = infer_source_name(&url);
        assert_eq!(name, "svc-9090-metrics-v2-tenant-a");
        assert!(!name.contains('/') && !name.contains("--"));
        assert!(!name.starts_with('-') && !name.ends_with('-'));
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
