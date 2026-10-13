//! The recorder's [`Probe`] hook for Rezolus agents.
//!
//! The rezolus half of detection and identity (see `detect`): recognising a
//! Rezolus agent by `/status`, else the `/` banner every agent serves;
//! wording the refusal of one that cannot stream; and fetching what an agent
//! does not yet put in its handshake (`host` from `/systeminfo`, `version`,
//! `systeminfo`, `descriptions`).

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::{Client, Url};

use super::detect::{At, Enrich, Probe, StreamAnswer, Verdict};
use super::RELEASE_5X;
use crate::parquet_metadata::{KEY_DESCRIPTIONS, KEY_SYSTEMINFO, KEY_VERSION};

/// The first Rezolus release whose agent serves `/metrics/stream`.
pub(crate) const STREAM_SINCE: &str = "5.21.0";

/// The hook `record` detects and enriches endpoints with.
pub(crate) struct RezolusProbe {
    client: Client,
}

impl RezolusProbe {
    pub(crate) fn new(client: Client) -> Self {
        Self { client }
    }
}

/// One GET of a metadata route.
enum Fetched {
    /// A success, with its body.
    Body(String),
    /// An answer that says the route is not there or not this (a 404, a
    /// 403, a body that does not read), which asking again will not change.
    Answered,
    /// No answer, or one that may change (a 5xx, 408 or 429).
    Unanswered,
}

impl Fetched {
    fn answered(&self) -> bool {
        !matches!(self, Fetched::Unanswered)
    }

    fn body(self) -> Option<String> {
        match self {
            Fetched::Body(body) => Some(body),
            _ => None,
        }
    }
}

async fn fetch(client: &Client, base: &Url, path: &str) -> Fetched {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(None);
    let Ok(response) = client.get(url).send().await else {
        return Fetched::Unanswered;
    };
    let status = response.status();
    if status.is_success() {
        return match response.text().await {
            Ok(body) => Fetched::Body(body),
            Err(_) => Fetched::Answered,
        };
    }
    if StreamAnswer::Status(status.as_u16()).is_transient() {
        Fetched::Unanswered
    } else {
        Fetched::Answered
    }
}

/// What the endpoint is, as far as `/status` and `/` say.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Identified {
    /// A Rezolus agent. `version` from `/status` or the `/` banner;
    /// `epoch` from `/status` only (an agent old enough to lack `/status`
    /// predates the epoch).
    Agent {
        version: Option<String>,
        epoch: Option<String>,
    },
    /// One of the two answered, and neither says it is an agent.
    Other,
    /// Neither answered.
    Unanswered,
}

/// Recognise a Rezolus agent at `base`. `/status` is the structured answer
/// and is tried first; it is recent (5.16.0), so an agent that predates it is
/// recognised by the `/` banner, `"Rezolus <version> Agent"`, which every
/// agent this project has shipped serves.
pub(crate) async fn identify(client: &Client, base: &Url) -> Identified {
    let status = fetch(client, base, "/status").await;
    let status_answered = status.answered();
    if let Some(body) = status.body() {
        if let Ok(status) = serde_json::from_str::<crate::agent::sampler_status::AgentStatus>(&body)
        {
            let epoch = (!status.producer_epoch.is_empty()).then_some(status.producer_epoch);
            let version = (!status.version.is_empty()).then_some(status.version);
            if version.is_some() || epoch.is_some() {
                return Identified::Agent { version, epoch };
            }
        }
    }
    let root = fetch(client, base, "/").await;
    let root_answered = root.answered();
    if let Some(version) = root.body().as_deref().and_then(parse_root_version) {
        return Identified::Agent {
            version: Some(version),
            epoch: None,
        };
    }
    if status_answered || root_answered {
        Identified::Other
    } else {
        Identified::Unanswered
    }
}

/// Pull the version out of the agent's root page, whose first line is
/// `Rezolus <version> Agent`. Returns `None` for anything else, so a proxy's
/// error page or another service on the port is not taken for an agent.
pub(crate) fn parse_root_version(body: &str) -> Option<String> {
    let first = body.lines().next()?;
    let rest = first.strip_prefix("Rezolus ")?;
    let version = rest.strip_suffix(" Agent")?;
    if version.is_empty() || version.contains(char::is_whitespace) {
        return None;
    }
    Some(version.to_string())
}

#[async_trait::async_trait]
impl Probe for RezolusProbe {
    /// A Rezolus agent is never scraped by 6.x: one whose stream route gave a
    /// transient answer is retried, any other answer refuses it. Anything
    /// that answered `/status` or `/` and is not an agent is scraped as
    /// Prometheus; when neither answered, nothing is known yet.
    async fn not_streamable(&self, url: &Url, at: At, answer: &StreamAnswer) -> Verdict {
        match identify(&self.client, url).await {
            Identified::Agent { version, .. } => {
                if answer.is_transient() {
                    Verdict::Pending
                } else {
                    let mut stream_url = url.clone();
                    stream_url.set_path("/metrics/stream");
                    stream_url.set_query(None);
                    let reason = format!("{stream_url} answered {answer}");
                    Verdict::Refuse {
                        reason: unstreamable_agent(url, version.as_deref(), &reason, at),
                    }
                }
            }
            Identified::Other => Verdict::Scrape,
            Identified::Unanswered => Verdict::Pending,
        }
    }

    /// `host` (from `/systeminfo`'s hostname) and, for a recognised agent,
    /// `source=rezolus`; metadata `version`, `systeminfo` and `descriptions`
    /// where the agent served them. `Retry` when no route answered.
    async fn enrich(&self, url: &Url, _epoch: Option<&str>) -> Enrich {
        let (systeminfo, descriptions, identified) = tokio::join!(
            fetch(&self.client, url, "/systeminfo"),
            fetch(&self.client, url, "/metrics/descriptions"),
            identify(&self.client, url),
        );
        if !systeminfo.answered()
            && !descriptions.answered()
            && identified == Identified::Unanswered
        {
            return Enrich::Retry;
        }
        let mut labels = BTreeMap::new();
        let mut metadata = BTreeMap::new();
        let (version, epoch) = match identified {
            Identified::Agent { version, epoch } => {
                labels.insert("source".to_string(), "rezolus".to_string());
                (version, epoch)
            }
            _ => (None, None),
        };
        if let Some(version) = version {
            metadata.insert(KEY_VERSION.to_string(), version);
        }
        if let Some(json) = systeminfo.body() {
            if let Some(host) = super::rez::host_from_systeminfo(&json) {
                labels.insert("host".to_string(), host);
            }
            metadata.insert(KEY_SYSTEMINFO.to_string(), json);
        }
        if let Some(json) = descriptions.body() {
            metadata.insert(KEY_DESCRIPTIONS.to_string(), json);
        }
        Enrich::Metadata {
            epoch,
            labels,
            metadata,
        }
    }

    fn merge(&self, key: &str, old: &str, new: &str) -> String {
        merge(key, old, new)
    }
}

/// How a restarted agent's metadata value replaces the one on record:
/// `descriptions` is the union of the two maps, the new description winning
/// for a metric in both, so the rows from before the restart keep theirs.
/// Every other key takes the new value.
pub(crate) fn merge(key: &str, old: &str, new: &str) -> String {
    if key != KEY_DESCRIPTIONS {
        return new.to_string();
    }
    use serde_json::{Map, Value};
    let parse = |s: &str| serde_json::from_str::<Map<String, Value>>(s).ok();
    match (parse(old), parse(new)) {
        (Some(mut merged), Some(new_map)) => {
            merged.extend(new_map);
            Value::Object(merged).to_string()
        }
        _ => new.to_string(),
    }
}

/// What the restarted agent at `base_url` reports about itself, fetched
/// within `timeout`: `None` when nothing answered, or when it describes a
/// process other than `epoch`'s (the agent restarted again). The agent's
/// `version`, `systeminfo` and `descriptions`, through [`RezolusProbe`]'s
/// `enrich`.
pub(crate) async fn fetch_restarted_agent(
    client: &Client,
    base_url: &Url,
    epoch: &str,
    timeout: Duration,
) -> Option<super::restart::Agent> {
    let probe = RezolusProbe::new(client.clone());
    let (_, metadata) = super::detect::enrich_within(&probe, base_url, Some(epoch), timeout)
        .await
        .ok()?;
    let agent = super::restart::Agent::from_metadata(&metadata);
    (agent.version.is_some() || agent.systeminfo.is_some() || agent.descriptions.is_some())
        .then_some(agent)
}

/// The refusal for a Rezolus agent that cannot serve its replication stream.
///
/// Names the agent's version when `/status` or the `/` banner gave one. An
/// agent older than [`STREAM_SINCE`] is said to predate the stream; one at or
/// after it should serve the stream, so the refusal only quotes what the
/// route answered (a proxy that does not route `/metrics/stream` gives a 404
/// from a current agent). There is no scrape fallback: 6.x records agents by
/// stream only, and points at a 5.x recorder, which scrapes them.
pub(crate) fn unstreamable_agent(url: &Url, version: Option<&str>, reason: &str, at: At) -> String {
    let mut stream_url = url.clone();
    stream_url.set_path("/metrics/stream");
    stream_url.set_query(None);
    let why = match version {
        Some(version) => match predates_stream(version) {
            Some(true) => format!(
                "{url} is Rezolus {version}, which predates the replication stream \
                 (agents serve /metrics/stream from {STREAM_SINCE}): {reason}"
            ),
            _ => format!(
                "{url} is Rezolus {version}, which serves a replication stream, but \
                 {reason}. Check that {stream_url} is reachable from this host; a proxy in \
                 front of the agent may not route it"
            ),
        },
        None => format!(
            "{url} is a Rezolus agent of unknown version, and its replication stream \
             did not open: {reason}. Agents serve /metrics/stream from {STREAM_SINCE}"
        ),
    };
    let upgrade = match version.and_then(predates_stream) {
        Some(false) => String::new(),
        _ => format!("; or upgrade the agent to {STREAM_SINCE} or later"),
    };
    let fallback = format!(
        "rezolus 6.x records Rezolus agents from their replication stream only. A \
         rezolus 5.x recorder ({RELEASE_5X} or later) records this \
         agent by scraping it, and `rezolus recording upgrade --to dendro in.rez -o \
         out.dendro` converts its .rez afterwards{upgrade}"
    );
    let next = match at {
        At::Startup => "Nothing was recorded",
        At::Activation => {
            "This endpoint is excluded from this recording and not retried; the other \
             endpoints keep recording"
        }
        At::Reconnect => RECONNECT_REFUSED,
    };
    format!("{why}. {next}. {fallback}")
}

/// What happens to an endpoint refused on a reconnect.
pub(crate) const RECONNECT_REFUSED: &str =
    "This endpoint's recording stops here, keeping the rows it had, and is not retried; the \
     other endpoints keep recording";

/// Whether `version` (as `/status` or the `/` banner reports it) is older
/// than [`STREAM_SINCE`]. `None` when it does not parse as
/// `major.minor.patch`; a pre-release or build suffix is ignored, so
/// `5.21.0-alpha.1` counts as 5.21.0.
pub(crate) fn predates_stream(version: &str) -> Option<bool> {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let triple = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(triple)
    };
    Some(parse(version)? < parse(STREAM_SINCE)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_version_parses_the_agent_banner() {
        assert_eq!(
            parse_root_version("Rezolus 5.20.0 Agent\nFor information, see: https://rezolus.com\n")
                .as_deref(),
            Some("5.20.0")
        );
        // A prerelease is the interesting case: most of the sampler history
        // worth bisecting lives in `-alpha.N` builds.
        assert_eq!(
            parse_root_version("Rezolus 5.20.1-alpha.0 Agent\n").as_deref(),
            Some("5.20.1-alpha.0")
        );
    }

    #[test]
    fn root_version_refuses_anything_that_is_not_the_banner() {
        // A proxy error page, another service on the port, and a banner with
        // no version in it. None of these may be recorded as a version.
        assert_eq!(parse_root_version("<html>404</html>"), None);
        assert_eq!(parse_root_version("Prometheus 2.5\n"), None);
        assert_eq!(parse_root_version("Rezolus  Agent\n"), None);
        assert_eq!(parse_root_version("Rezolus 5.20.0 Exporter\n"), None);
        assert_eq!(parse_root_version(""), None);
    }

    /// The refusal names the agent and what its stream route answered, the
    /// agent's version when there is one, and points at a 5.x recorder, which
    /// scrapes it. Only an agent older than the stream is said to predate it
    /// (and told to upgrade): a current agent's 404 is quoted as the route's
    /// answer.
    #[test]
    fn an_unstreamable_agent_is_refused_by_version_pointing_at_a_5x_recorder() {
        let url: Url = "http://localhost:4241".parse().unwrap();
        let reason = "http://localhost:4241/metrics/stream answered HTTP 404";
        let msg = unstreamable_agent(&url, Some("5.20.0"), reason, At::Startup);
        for needle in [
            "http://localhost:4241/ is Rezolus 5.20.0",
            "predates",
            reason,
            STREAM_SINCE,
            "Nothing was recorded",
            "5.x recorder",
            RELEASE_5X,
            "recording upgrade --to dendro",
            "upgrade the agent",
        ] {
            assert!(msg.contains(needle), "{needle:?} in {msg}");
        }
        for gone in ["-o out.rez", "-o out.parquet"] {
            assert!(!msg.contains(gone), "{gone:?} in {msg}");
        }

        let msg = unstreamable_agent(&url, Some("5.22.1"), reason, At::Startup);
        assert!(!msg.contains("predates"), "{msg}");
        assert!(!msg.contains("upgrade the agent"), "{msg}");
        assert!(
            msg.contains("Rezolus 5.22.1, which serves") && msg.contains(reason),
            "{msg}"
        );
        assert!(
            msg.contains("Check that http://localhost:4241/metrics/stream is reachable"),
            "{msg}"
        );
        assert!(msg.contains("5.x recorder"), "{msg}");

        let msg = unstreamable_agent(&url, None, reason, At::Startup);
        assert!(msg.contains("unknown version"), "{msg}");
        assert!(msg.contains(STREAM_SINCE), "{msg}");
        assert!(msg.contains("5.x recorder"), "{msg}");

        // On activation and on a reconnect, only the endpoint is left out.
        let msg = unstreamable_agent(&url, Some("5.20.0"), reason, At::Activation);
        assert!(msg.contains("excluded from this recording"), "{msg}");
        assert!(msg.contains("5.x recorder"), "{msg}");
        let msg = unstreamable_agent(&url, Some("5.20.0"), reason, At::Reconnect);
        assert!(msg.contains("recording stops here"), "{msg}");
        assert!(msg.contains("5.x recorder"), "{msg}");
    }

    #[test]
    fn predates_stream_compares_the_release() {
        assert_eq!(predates_stream("5.20.0"), Some(true));
        assert_eq!(predates_stream("4.1.2"), Some(true));
        assert_eq!(predates_stream("5.21.0"), Some(false));
        assert_eq!(predates_stream("5.21.0-alpha.1"), Some(false));
        assert_eq!(predates_stream("6.0.0-alpha.14"), Some(false));
        assert_eq!(predates_stream("5.21"), None);
        assert_eq!(predates_stream("dev"), None);
    }

    // ── detection against in-process producers ──────────────────────────────

    use crate::recorder::detect::{self, Detected, Transport};
    use std::sync::{Arc, Mutex};

    /// What a fake producer serves.
    #[derive(Clone)]
    enum Fake {
        /// A Prometheus exporter: `/metrics` and a 404 for everything else,
        /// `/` included.
        Prometheus,
        /// An agent older than the stream: the `/` banner, a 404 for the
        /// stream route and `/status`.
        OldAgent(&'static str),
        /// A current agent: a stream, `/status`, `/systeminfo`.
        Agent,
        /// A producer serving only the stream route, whose handshake carries
        /// these labels and metadata.
        StreamOnly(
            Vec<(&'static str, &'static str)>,
            Vec<(&'static str, &'static str)>,
        ),
        /// 503 for everything, as a proxy whose backend is down answers.
        Failing,
    }

    fn opening(
        epoch: &str,
        labels: &[(&'static str, &'static str)],
        metadata: &[(&'static str, &'static str)],
    ) -> Vec<u8> {
        let map = |pairs: &[(&'static str, &'static str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let mut bytes = Vec::new();
        dendro::replicate::wire::write_preamble(&mut bytes).unwrap();
        dendro::replicate::wire::encode_frame(
            &dendro::replicate::Frame::Handshake {
                source: 0,
                uuid: Some(epoch.to_string()),
                labels: map(labels),
                metadata: map(metadata),
                clock_anchor_wall_ns: 1_700_000_000_000_000_000,
                complete: false,
            },
            &mut bytes,
        )
        .unwrap();
        bytes
    }

    fn answer(fake: &Fake, path: &str) -> axum::response::Response {
        use axum::http::{header::CONTENT_TYPE, StatusCode};
        use axum::response::IntoResponse;
        let stream = |labels: &[(&'static str, &'static str)],
                      metadata: &[(&'static str, &'static str)]| {
            (
                [(
                    CONTENT_TYPE,
                    metriken_exposition::stream::WIRE_NAMES.content_type,
                )],
                opening("epoch-1", labels, metadata),
            )
                .into_response()
        };
        match (fake, path) {
            (Fake::Failing, _) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            (Fake::Prometheus, "/metrics") => "# TYPE up gauge\nup 1\n".into_response(),
            (Fake::OldAgent(version), "/") => format!("Rezolus {version} Agent\n").into_response(),
            (Fake::Agent, "/metrics/stream") => stream(&[("source", "rezolus")], &[]),
            (Fake::Agent, "/status") => r#"{"version":"6.0.0","producer_epoch":"epoch-1",
                "uptime_seconds":1,"ttl_seconds":1,"samplers":[]}"#
                .into_response(),
            (Fake::Agent, "/systeminfo") => r#"{"hostname":"web-01"}"#.into_response(),
            (Fake::Agent, "/") => "Rezolus 6.0.0 Agent\n".into_response(),
            (Fake::StreamOnly(labels, metadata), "/metrics/stream") => stream(labels, metadata),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// Serve `fake` on a free port; the returned handle changes what it
    /// serves from the next request on.
    async fn serve(fake: Fake) -> (Url, Arc<Mutex<Fake>>) {
        let state = Arc::new(Mutex::new(fake));
        let shared = state.clone();
        let router = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let fake = shared.lock().unwrap().clone();
            async move { answer(&fake, uri.path()) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (format!("http://{addr}").parse().unwrap(), state)
    }

    fn client() -> Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Client::builder().http1_only().build().unwrap()
    }

    fn cfg() -> detect::DetectConfig {
        detect::DetectConfig {
            interval: Duration::from_millis(100),
            timeout: Duration::from_secs(2),
            layout: crate::recorder::stream::Layout::Long,
            wire_names: crate::recorder::stream::ACCEPTED_WIRE_NAMES,
        }
    }

    async fn detect_at(url: &Url, transport: Transport, at: At) -> Detected {
        let client = client();
        let probe = RezolusProbe::new(client.clone());
        detect::detect(&client, &cfg(), url, transport, &probe, at).await
    }

    /// A Prometheus exporter given by its root URL is scraped at `/metrics`:
    /// its stream route 404s, and `/status` and `/` answer without naming an
    /// agent.
    #[tokio::test]
    async fn a_prometheus_endpoint_at_the_root_is_scraped_at_metrics() {
        let (url, _) = serve(Fake::Prometheus).await;
        match detect_at(&url, Transport::Auto, At::Startup).await {
            Detected::Scraped(scrape) => assert_eq!(scrape.path(), "/metrics"),
            other => panic!("expected a scrape, got {}", describe(&other)),
        }
    }

    /// A producer serving only `/metrics/stream` (no `/metrics/binary`, no
    /// `/status`) is streamed, and the hook does not claim it is an agent.
    #[tokio::test]
    async fn a_stream_only_producer_is_streamed_with_its_handshake_identity() {
        let (url, _) = serve(Fake::StreamOnly(
            vec![("source", "cachecannon")],
            vec![("version", "0.3.0")],
        ))
        .await;
        let sub = match detect_at(&url, Transport::Auto, At::Startup).await {
            Detected::Streamed(sub) => sub,
            other => panic!("expected a stream, got {}", describe(&other)),
        };
        let source = sub.source().unwrap();
        assert_eq!(source.labels["source"], "cachecannon");
        assert_eq!(source.metadata["version"], "0.3.0");

        let probe = RezolusProbe::new(client());
        let (labels, metadata) =
            detect::enrich_within(&probe, &url, Some("epoch-1"), Duration::from_secs(2))
                .await
                .expect("the producer answered, so the hook has a reply");
        assert!(
            labels.is_empty() && metadata.is_empty(),
            "{labels:?} {metadata:?}"
        );
    }

    /// An agent older than the stream is refused, by version, at startup and
    /// on activation, with the wording each calls for. Never scraped.
    #[tokio::test]
    async fn an_agent_without_a_stream_is_refused_by_version() {
        let (url, _) = serve(Fake::OldAgent("5.20.0")).await;
        for (at, next) in [
            (At::Startup, "Nothing was recorded"),
            (At::Activation, "excluded from this recording"),
        ] {
            match detect_at(&url, Transport::Auto, at).await {
                Detected::Refused(reason) => {
                    for needle in ["Rezolus 5.20.0", "predates", "answered HTTP 404", next] {
                        assert!(reason.contains(needle), "{needle:?} in {reason}");
                    }
                }
                other => panic!("expected a refusal, got {}", describe(&other)),
            }
        }
    }

    /// An endpoint that answers 503 to everything is pending, not scraped and
    /// not refused; once it answers as an agent it is streamed.
    #[tokio::test]
    async fn an_endpoint_answering_5xx_is_pending_until_it_streams() {
        let (url, fake) = serve(Fake::Failing).await;
        match detect_at(&url, Transport::Auto, At::Startup).await {
            Detected::Pending(why) => assert!(why.contains("HTTP 503"), "{why}"),
            other => panic!("expected pending, got {}", describe(&other)),
        }
        *fake.lock().unwrap() = Fake::Agent;
        match detect_at(&url, Transport::Auto, At::Activation).await {
            Detected::Streamed(_) => {}
            other => panic!("expected a stream, got {}", describe(&other)),
        }
    }

    /// Nothing listening is pending.
    #[tokio::test]
    async fn an_unreachable_endpoint_is_pending() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url: Url = format!("http://127.0.0.1:{port}").parse().unwrap();
        assert!(matches!(
            detect_at(&url, Transport::Auto, At::Startup).await,
            Detected::Pending(_)
        ));
    }

    /// `protocol=stream` is never scraped, and `protocol=prometheus` never
    /// asked for a stream.
    #[tokio::test]
    async fn a_declared_protocol_overrides_detection() {
        let (prom, _) = serve(Fake::Prometheus).await;
        match detect_at(&prom, Transport::Stream, At::Startup).await {
            Detected::Refused(reason) => assert!(reason.contains("protocol=stream"), "{reason}"),
            other => panic!("expected a refusal, got {}", describe(&other)),
        }
        let (agent, _) = serve(Fake::Agent).await;
        match detect_at(&agent, Transport::Prometheus, At::Startup).await {
            Detected::Pending(why) => assert!(why.contains("HTTP 404"), "{why}"),
            other => panic!("expected pending, got {}", describe(&other)),
        }
    }

    /// A current agent's `enrich`: `source=rezolus`, `host` from its
    /// systeminfo, and its version, and a reply about another process is
    /// discarded.
    #[tokio::test]
    async fn an_agents_enrich_names_it_and_its_host() {
        let (url, _) = serve(Fake::Agent).await;
        let probe = RezolusProbe::new(client());
        let (labels, metadata) =
            detect::enrich_within(&probe, &url, Some("epoch-1"), Duration::from_secs(2))
                .await
                .unwrap();
        assert_eq!(labels["source"], "rezolus");
        assert_eq!(labels["host"], "web-01");
        assert_eq!(metadata[KEY_VERSION], "6.0.0");
        assert!(
            detect::enrich_within(&probe, &url, Some("epoch-2"), Duration::from_secs(2))
                .await
                .is_err(),
            "a reply naming another epoch is discarded"
        );
    }

    fn describe(d: &Detected) -> String {
        match d {
            Detected::Streamed(_) => "a stream".to_string(),
            Detected::Scraped(url) => format!("a scrape at {url}"),
            Detected::Refused(r) => format!("a refusal: {r}"),
            Detected::Pending(w) => format!("pending: {w}"),
        }
    }

    #[test]
    fn descriptions_merge_as_a_union_and_other_keys_take_the_new_value() {
        let merged: serde_json::Value = serde_json::from_str(&merge(
            KEY_DESCRIPTIONS,
            r#"{"a":"old a","b":"old b"}"#,
            r#"{"b":"new b","c":"new c"}"#,
        ))
        .unwrap();
        assert_eq!(
            merged,
            serde_json::json!({"a": "old a", "b": "new b", "c": "new c"})
        );
        assert_eq!(merge(KEY_DESCRIPTIONS, "not json", "{}"), "{}");
        assert_eq!(merge(KEY_VERSION, "1", "2"), "2");
    }
}
