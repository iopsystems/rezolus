//! How an endpoint is recorded, and whose word its identity takes.
//!
//! The generic half of metriken's recorder design
//! (`docs/journal/2026-10-12-metriken-recorder.md` in metriken: Detection,
//! Source identity): what is asked of a producer, in what order, and how the
//! answers combine. What a Rezolus agent is, and what to say when one cannot
//! stream, is the [`Probe`] hook's, implemented in `agent_probe`. Nothing
//! here names a Rezolus type, so the module moves into `metriken-recorder`
//! as it is.
//!
//! # Detection
//!
//! An endpoint whose path is `/` or `/metrics/stream` is asked for its stream
//! first. A handshake in an accepted content type means it is streamed.
//! Anything else (a 404, a 409, a foreign content type, a 5xx, a timeout, a
//! connection error) goes to [`Probe::not_streamable`] with the route's
//! [`StreamAnswer`], whose [`Verdict`] decides: scrape it as Prometheus at
//! `/metrics`, refuse it, or try again next tick. An endpoint with any other
//! path is scraped at its URL. `protocol=` on the endpoint overrides the
//! path: `stream` (and `msgpack`, `rezolus`) is only ever streamed,
//! `prometheus` only ever scraped.
//!
//! # Identity
//!
//! Labels, highest first: the user's, the hook's ([`Probe::enrich`]), the
//! stream handshake's. `source` is ordered on its own: the user's, the
//! handshake's, the hook's (a hook sets it when it recognises the producer),
//! a name inferred from the URL. Metadata, highest first: the user's, the
//! recorder's own keys, the hook's, the handshake's.

use std::collections::BTreeMap;
use std::time::Duration;

use metriken_exposition::stream::WireNames;
use reqwest::{Client, Url};

pub(crate) use super::stream::StreamAnswer;
use super::stream::{Layout, Subscription};

/// What to do with an endpoint whose stream route did not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not a producer that streams: scrape it as Prometheus.
    Scrape,
    /// A producer that should stream and cannot. `reason` is the whole line
    /// the user is shown.
    Refuse { reason: String },
    /// Not known yet; ask again next tick.
    Pending,
}

/// When the hook is asked, which decides how a refusal is worded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    /// Before the archive exists: a refusal ends the run.
    Startup,
    /// After the archive opened, on an endpoint's first activation: a
    /// refusal leaves this endpoint out.
    Activation,
    /// On a reconnect, after the stream had been open: the endpoint's
    /// recording stops with the rows it had.
    Reconnect,
}

/// What a producer says about itself beyond its handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Enrich {
    Metadata {
        /// The producer's epoch as the hook read it. A reply whose epoch
        /// differs from the handshake's uuid is about another process and
        /// is discarded ([`accept`]).
        epoch: Option<String>,
        labels: BTreeMap<String, String>,
        metadata: BTreeMap<String, String>,
    },
    /// Nothing answered; ask again later.
    Retry,
}

/// Labels and metadata from the probe hook, as taken by [`accept`].
pub(crate) type Enriched = (BTreeMap<String, String>, BTreeMap<String, String>);

/// What a recorder asks of the caller about a producer it cannot classify
/// from the stream route alone.
#[async_trait::async_trait]
pub(crate) trait Probe: Send + Sync {
    /// The stream route at `url` answered `answer` rather than a handshake.
    /// Called at detection ([`At::Startup`], [`At::Activation`]) for the
    /// verdict, and on a reconnect ([`At::Reconnect`]) for the reason text
    /// only: a streamed source is never switched to a scrape, and a verdict
    /// other than `Refuse` there gives the route's answer as the reason.
    async fn not_streamable(&self, url: &Url, at: At, answer: &StreamAnswer) -> Verdict;

    /// Labels and metadata for the streamed producer at `url`, whose
    /// handshake named `epoch`. Runs after the handshake, so it never sets
    /// the epoch or the clock anchor.
    async fn enrich(&self, url: &Url, epoch: Option<&str>) -> Enrich;

    /// Combine a metadata value the hook owns when a restarted producer
    /// reports a new one.
    fn merge(&self, _key: &str, _old: &str, new: &str) -> String {
        new.to_string()
    }
}

/// How an endpoint was declared: `protocol=` on `--endpoint`, or nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    Auto,
    Stream,
    Prometheus,
}

/// The subscription settings detection opens a stream with, so the stream it
/// hands on is the one the recording reads.
pub(crate) struct DetectConfig {
    pub interval: Duration,
    /// Bounds the stream connect, the hook's call, and the reachability check
    /// of a scraped URL, each on its own.
    pub timeout: Duration,
    pub layout: Layout,
    pub wire_names: &'static [WireNames],
}

/// How one endpoint is to be recorded.
pub(crate) enum Detected {
    /// Streamed, over this open subscription (its handshake applied).
    Streamed(Box<Subscription>),
    /// Scraped as Prometheus at this URL.
    Scraped(Url),
    /// Cannot be recorded; the line that says why.
    Refused(String),
    /// Not known yet; the line that says why. Detected again next tick.
    Pending(String),
}

/// Whether `url` is asked for its stream first under `Transport::Auto`.
fn streams_first(url: &Url) -> bool {
    matches!(url.path(), "/" | "/metrics/stream")
}

/// Detect how to record the endpoint at `url`. See the module docs.
pub(crate) async fn detect(
    client: &Client,
    cfg: &DetectConfig,
    url: &Url,
    transport: Transport,
    probe: &dyn Probe,
    at: At,
) -> Detected {
    match transport {
        Transport::Prometheus => {
            let mut scrape = url.clone();
            if scrape.path() == "/" {
                scrape.set_path("/metrics");
            }
            check_scrape(client, cfg, scrape).await
        }
        Transport::Auto if !streams_first(url) => check_scrape(client, cfg, url.clone()).await,
        Transport::Auto | Transport::Stream => {
            let (answer, message) = match tokio::time::timeout(
                cfg.timeout,
                Subscription::connect_answered(
                    client,
                    url,
                    cfg.interval,
                    cfg.layout,
                    cfg.wire_names,
                ),
            )
            .await
            {
                Ok(Ok(sub)) => return Detected::Streamed(Box::new(sub)),
                Ok(Err(refusal)) => (refusal.answer, refusal.message),
                Err(_) => (
                    StreamAnswer::Timeout,
                    format!(
                        "{url} did not complete a handshake within {}",
                        humantime::format_duration(cfg.timeout)
                    ),
                ),
            };
            let verdict = tokio::time::timeout(cfg.timeout, probe.not_streamable(url, at, &answer))
                .await
                .unwrap_or(Verdict::Pending);
            match verdict {
                Verdict::Refuse { reason } => Detected::Refused(reason),
                Verdict::Pending => Detected::Pending(format!(
                    "endpoint {url}: its replication stream did not open ({message}); will \
                     retry each tick"
                )),
                Verdict::Scrape if transport == Transport::Auto => {
                    let mut scrape = url.clone();
                    scrape.set_path("/metrics");
                    scrape.set_query(None);
                    Detected::Scraped(scrape)
                }
                // Declared `protocol=stream`: never scraped instead.
                Verdict::Scrape if answer.is_transient() => Detected::Pending(format!(
                    "endpoint {url}: its replication stream did not open ({message}); will \
                     retry each tick"
                )),
                Verdict::Scrape => Detected::Refused(format!(
                    "{message}. {url} is declared protocol=stream, so it is not scraped \
                     instead; declare protocol=prometheus to scrape it"
                )),
            }
        }
    }
}

/// A scraped endpoint is active once its URL answers with a success; until
/// then it is pending, as an unreachable endpoint always was.
async fn check_scrape(client: &Client, cfg: &DetectConfig, url: Url) -> Detected {
    let answered = tokio::time::timeout(cfg.timeout, client.get(url.clone()).send()).await;
    match answered {
        Ok(Ok(resp)) if resp.status().is_success() => Detected::Scraped(url),
        Ok(Ok(resp)) => Detected::Pending(format!(
            "endpoint {url} answered HTTP {}, will retry each tick",
            resp.status().as_u16()
        )),
        Ok(Err(_)) | Err(_) => Detected::Pending(format!(
            "endpoint {url} not reachable, will retry each tick"
        )),
    }
}

/// An [`Enrich`] reply as labels and metadata, or `None` when there is none
/// to take: `Retry`, or a reply about a process other than `epoch`'s (the
/// producer restarted between the handshake and the hook's reads). A reply
/// with no epoch cannot be checked and is taken.
pub(crate) fn accept(reply: Enrich, epoch: Option<&str>) -> Option<Enriched> {
    match reply {
        Enrich::Retry => None,
        Enrich::Metadata {
            epoch: Some(seen), ..
        } if epoch.is_some_and(|e| e != seen) => None,
        Enrich::Metadata {
            labels, metadata, ..
        } => Some((labels, metadata)),
    }
}

/// [`Probe::enrich`] bounded by `timeout`, taken through [`accept`]. `Err`
/// says why there is nothing to take, for the warning that the source was
/// added without the hook's labels.
pub(crate) async fn enrich_within(
    probe: &dyn Probe,
    url: &Url,
    epoch: Option<&str>,
    timeout: Duration,
) -> Result<Enriched, String> {
    match tokio::time::timeout(timeout, probe.enrich(url, epoch)).await {
        Err(_) => Err(format!(
            "its metadata did not arrive within {}",
            humantime::format_duration(timeout)
        )),
        Ok(Enrich::Retry) => Err("its metadata routes did not answer".to_string()),
        Ok(reply) => accept(reply, epoch)
            .ok_or_else(|| "its metadata describes another process (it restarted)".to_string()),
    }
}

/// The identity a recording is added with: its labels and metadata, from
/// the four places they can come from.
pub(crate) struct Identity<'a> {
    /// `--endpoint url,source=` (or `--url` with `--metadata source=`).
    pub user_source: Option<&'a str>,
    /// `--label k=v`, for every endpoint.
    pub user_labels: &'a [(String, String)],
    /// `--metadata k=v`, for every endpoint.
    pub user_metadata: &'a [(String, String)],
    /// Keys the recorder writes itself (`sampling_interval_ms`,
    /// `producer_epoch`, `role`, run events).
    pub recorder_metadata: BTreeMap<String, String>,
    pub hook_labels: &'a BTreeMap<String, String>,
    pub hook_metadata: &'a BTreeMap<String, String>,
    pub handshake_labels: &'a BTreeMap<String, String>,
    pub handshake_metadata: &'a BTreeMap<String, String>,
    pub url: &'a Url,
}

impl Identity<'_> {
    /// The recording's `source`: the user's, the handshake's, the hook's, or
    /// a name inferred from the URL.
    pub(crate) fn source(&self) -> String {
        self.user_source
            .map(str::to_string)
            .or_else(|| self.handshake_labels.get("source").cloned())
            .or_else(|| self.hook_labels.get("source").cloned())
            .unwrap_or_else(|| super::endpoint::infer_source_name(self.url))
    }

    /// Labels: the handshake's, under the hook's, under the user's, with
    /// `source` from [`source`](Self::source) unless a `--label source=`
    /// names one.
    pub(crate) fn labels(&self) -> BTreeMap<String, String> {
        let mut labels = self.handshake_labels.clone();
        labels.extend(self.hook_labels.clone());
        labels.insert("source".to_string(), self.source());
        for (k, v) in self.user_labels {
            labels.insert(k.clone(), v.clone());
        }
        labels
    }

    /// Metadata: the handshake's, under the hook's, under the recorder's
    /// keys and `source`, under the user's.
    pub(crate) fn metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = self.handshake_metadata.clone();
        metadata.extend(self.hook_metadata.clone());
        metadata.insert("source".to_string(), self.source());
        metadata.extend(self.recorder_metadata.clone());
        for (k, v) in self.user_metadata {
            metadata.insert(k.clone(), v.clone());
        }
        metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Labels: user over hook over handshake. `source`: user, handshake,
    /// hook, inferred.
    #[test]
    fn labels_take_the_user_then_the_hook_then_the_handshake() {
        let url: Url = "http://web-01:4241".parse().unwrap();
        let hook = map(&[
            ("source", "rezolus"),
            ("host", "from-hook"),
            ("arm", "hook"),
        ]);
        let handshake = map(&[
            ("source", "cachecannon"),
            ("host", "from-handshake"),
            ("zone", "a"),
        ]);
        let empty = BTreeMap::new();
        let user = pairs(&[("arm", "user")]);
        let id = Identity {
            user_source: None,
            user_labels: &user,
            user_metadata: &[],
            recorder_metadata: BTreeMap::new(),
            hook_labels: &hook,
            hook_metadata: &empty,
            handshake_labels: &handshake,
            handshake_metadata: &empty,
            url: &url,
        };
        assert_eq!(
            id.labels(),
            map(&[
                ("source", "cachecannon"),
                ("host", "from-hook"),
                ("arm", "user"),
                ("zone", "a"),
            ])
        );

        let id = Identity {
            user_source: Some("mine"),
            ..id
        };
        assert_eq!(id.source(), "mine");

        let no_handshake_source = map(&[("zone", "a")]);
        let id = Identity {
            user_source: None,
            handshake_labels: &no_handshake_source,
            ..id
        };
        assert_eq!(
            id.source(),
            "rezolus",
            "the hook's, when the handshake names none"
        );

        let id = Identity {
            hook_labels: &empty,
            ..id
        };
        assert_eq!(id.source(), "web-01-4241", "inferred from the URL last");
    }

    /// Metadata: user over the recorder's keys over the hook over the
    /// handshake.
    #[test]
    fn metadata_takes_the_user_then_the_recorder_then_the_hook_then_the_handshake() {
        let url: Url = "http://svc:8080".parse().unwrap();
        let hook = map(&[("version", "hook"), ("systeminfo", "{}")]);
        let handshake = map(&[
            ("version", "handshake"),
            ("producer_epoch", "handshake"),
            ("build", "abc"),
        ]);
        let empty = BTreeMap::new();
        let user = pairs(&[("systeminfo", "user")]);
        let id = Identity {
            user_source: None,
            user_labels: &[],
            user_metadata: &user,
            recorder_metadata: map(&[("producer_epoch", "recorder")]),
            hook_labels: &empty,
            hook_metadata: &hook,
            handshake_labels: &empty,
            handshake_metadata: &handshake,
            url: &url,
        };
        let m = id.metadata();
        assert_eq!(m["version"], "hook");
        assert_eq!(m["producer_epoch"], "recorder");
        assert_eq!(m["systeminfo"], "user");
        assert_eq!(m["build"], "abc");
        assert_eq!(m["source"], "svc-8080");
    }

    #[test]
    fn a_reply_about_another_process_is_discarded() {
        let reply = |epoch: Option<&str>| Enrich::Metadata {
            epoch: epoch.map(str::to_string),
            labels: BTreeMap::new(),
            metadata: map(&[("version", "1")]),
        };
        assert!(accept(reply(Some("a")), Some("a")).is_some());
        assert!(accept(reply(Some("b")), Some("a")).is_none());
        assert!(
            accept(reply(None), Some("a")).is_some(),
            "unchecked is taken"
        );
        assert!(accept(reply(Some("b")), None).is_some());
        assert!(accept(Enrich::Retry, Some("a")).is_none());
    }

    #[test]
    fn only_the_root_and_the_stream_route_are_asked_for_a_stream() {
        for (url, first) in [
            ("http://h:1", true),
            ("http://h:1/", true),
            ("http://h:1/metrics/stream", true),
            ("http://h:1/metrics", false),
            ("http://h:1/federate", false),
        ] {
            let url: Url = url.parse().unwrap();
            assert_eq!(streams_first(&url), first, "{url}");
        }
    }
}
