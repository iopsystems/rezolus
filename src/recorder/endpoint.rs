use reqwest::Url;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Recorded from its replication stream only, never scraped. Spelled
    /// `stream` in an endpoint's options; `rezolus` and `msgpack` (the 5.x
    /// spelling, which named an agent) mean the same.
    #[serde(alias = "rezolus", alias = "msgpack")]
    Stream,
    /// Scraped as Prometheus text only, at `/metrics` for a URL whose path
    /// is `/` and at the URL otherwise.
    Prometheus,
}

impl Protocol {
    /// The detection rule a declared protocol (or none) selects.
    pub fn transport(protocol: Option<&Protocol>) -> super::detect::Transport {
        match protocol {
            None => super::detect::Transport::Auto,
            Some(Protocol::Stream) => super::detect::Transport::Stream,
            Some(Protocol::Prometheus) => super::detect::Transport::Prometheus,
        }
    }
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
    /// The user's `source`. When `None`, the recording's `source` is resolved
    /// on activation: the stream handshake's, `rezolus` for a recognised
    /// agent, or a name inferred from the URL (see `detect::Identity`).
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
    /// Streamed and waiting for the probe hook's `enrich`, which runs off the
    /// tick; added to the archive on the first tick after it returns.
    Activating,
    /// Answered and cannot be recorded (an agent without a replication
    /// stream). Never retried.
    Refused,
}

/// What a streamed producer's handshake says about its process: the uuid and
/// the clock anchor. Both are the handshake's, never the probe hook's.
#[derive(Default)]
pub struct AgentMetadata {
    /// The producer epoch: an opaque id that changes when, and only when,
    /// every cumulative counter restarted from zero, which for a
    /// process-scoped producer is once per process. The stream handshake's
    /// uuid.
    pub producer_epoch: Option<String>,
    /// The wall clock the producer's timeline is anchored to, from the
    /// handshake.
    ///
    /// The recording is anchored here rather than on the recorder's own clock,
    /// because its rows carry the producer's timestamps: an anchor from one
    /// clock and rows from another would make `ts + wall_offset` resolve to a
    /// wall time neither party observed. `None` for a Prometheus source,
    /// which uses the recorder's anchor and its stamps together.
    pub clock_anchor_wall_ns: Option<i64>,
}

/// The labels and metadata a streamed endpoint's recording is added with,
/// besides the user's and the recorder's (see `detect::Identity`).
#[derive(Default)]
pub struct SourceIdentity {
    /// From the probe hook's `enrich`: for an agent, `source=rezolus` and
    /// `host`.
    pub hook_labels: BTreeMap<String, String>,
    /// From the probe hook's `enrich`: for an agent, `version`, `systeminfo`
    /// and `descriptions`.
    pub hook_metadata: BTreeMap<String, String>,
    /// From the stream handshake, the first connection's.
    pub handshake_labels: BTreeMap<String, String>,
    /// From the stream handshake, the latest connection's: a restarted
    /// producer's handshake replaces it.
    pub handshake_metadata: BTreeMap<String, String>,
}

/// Runtime state for a single endpoint during recording.
pub struct EndpointState {
    pub config: EndpointConfig,
    pub status: EndpointStatus,
    pub detected_protocol: Option<Protocol>,
    pub scrape_url: Option<Url>,
    /// The epoch and anchor of a streamed endpoint's handshake. All-`None`
    /// until the endpoint streams; a Prometheus endpoint never fills it.
    pub agent: AgentMetadata,
    /// Where a streamed endpoint's labels and metadata come from.
    pub identity: SourceIdentity,
    pub first_success_ns: Option<u64>,
    pub last_success_ns: Option<u64>,
    /// True for a Rezolus agent, whose rows arrive off its replication stream
    /// rather than from a scrape each tick.
    pub streaming: bool,
    /// Stream intervals received so far (streamed endpoints only).
    pub frames: u64,
    /// Wall-clock stamp of the newest row in the last interval received,
    /// `ts + wall_offset` on the agent's clock; `None` when that interval
    /// carried no rows.
    pub last_frame_wall_ns: Option<u64>,
    /// Agent restarts seen and not yet written into the recording's
    /// metadata, oldest first. See `recorder::restart`.
    pub restarts: Vec<super::restart::Restart>,
    /// Fetches of the restarted agent's metadata that failed or found another
    /// process, for the restarts above.
    pub restart_fetches: u32,
    /// Whether any row has been staged into this endpoint's recording.
    pub rows_staged: bool,
}

impl EndpointState {
    pub fn new(config: EndpointConfig) -> Self {
        let detected_protocol = config.protocol.clone();
        Self {
            config,
            status: EndpointStatus::Pending,
            detected_protocol,
            scrape_url: None,
            agent: AgentMetadata::default(),
            identity: SourceIdentity::default(),
            first_success_ns: None,
            last_success_ns: None,
            streaming: false,
            frames: 0,
            last_frame_wall_ns: None,
            restarts: Vec::new(),
            restart_fetches: 0,
            rows_staged: false,
        }
    }

    /// A row stamped `ts` was just staged into this endpoint's recording: it
    /// is the first row of each restart still waiting for one.
    pub fn note_row_staged(&mut self, ts: u64) {
        self.rows_staged = true;
        for restart in &mut self.restarts {
            restart.from_ts.get_or_insert(ts);
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
    // Slashes and anything else awkward become `-`: this ends up as the
    // recording's `source` label value, which `--recording source=...`
    // selectors match, so it has to be a plain word.
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

    /// The inferred name becomes the recording's `source` label value, so it
    /// has to be a plain word — no slashes, no runs of separators, no leading
    /// or trailing dash.
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
            protocol: Some(Protocol::Stream),
        };
        let state = EndpointState::new(config);
        assert_eq!(state.status, EndpointStatus::Pending);
        assert_eq!(state.protocol(), Some(&Protocol::Stream));
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
