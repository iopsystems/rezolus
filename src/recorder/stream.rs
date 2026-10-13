//! An agent's replication stream, read through `metriken-recorder`.
//!
//! The frame decoder, the subscription and the pump are the library's
//! (`metriken_recorder::{Subscription, pump}`). What stays here is rezolus's:
//! the wire names agents from 5.21.0 through 6.0.0-alpha.44 serve their
//! stream under, and, for the agent's tests, the retry split of a connect.

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
pub(crate) use metriken_recorder::{Layout, Refusal, StreamEvent, StreamSubscriber, Subscription};
pub(crate) use metriken_recorder::{WireNames, WIRE_NAMES};

/// The names agents from 5.21.0 through 6.0.0-alpha.44 serve their stream
/// under. A later agent serves metriken's [`WIRE_NAMES`].
pub(crate) const REZOLUS_LEGACY_WIRE_NAMES: WireNames = WireNames {
    content_type: "application/vnd.rezolus.replication.v1+dendro",
    frame_interval: "x-rezolus-frame-interval",
    update_floor: "x-rezolus-update-floor",
    layout: "x-rezolus-layout",
};

/// The names this recorder accepts a stream under: metriken's, which current
/// agents serve, and the legacy names, so agents from 5.21.0 through
/// 6.0.0-alpha.44 are still recorded.
pub(crate) const ACCEPTED_WIRE_NAMES: &[WireNames] = &[WIRE_NAMES, REZOLUS_LEGACY_WIRE_NAMES];

#[cfg(test)]
/// Why a subscription could not be opened, split by whether trying again
/// could change the answer ([`StreamAnswer::is_transient`]).
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// Nothing answered, or the connection dropped before the handshake.
    Unreachable(String),
    /// Something answered, and it cannot serve a replication stream.
    Unsupported(String),
}

#[cfg(test)]
impl From<Refusal> for ConnectError {
    fn from(refusal: Refusal) -> Self {
        if refusal.answer.is_transient() {
            ConnectError::Unreachable(refusal.message)
        } else {
            ConnectError::Unsupported(refusal.message)
        }
    }
}

#[cfg(test)]
impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unreachable(e) | ConnectError::Unsupported(e) => f.write_str(e),
        }
    }
}

#[cfg(test)]
/// [`Subscription::connect`] under [`ACCEPTED_WIRE_NAMES`], with the answer
/// split by [`ConnectError`].
pub(crate) async fn connect(
    client: &reqwest::Client,
    base: &reqwest::Url,
    interval: Duration,
    layout: Layout,
) -> Result<Subscription, ConnectError> {
    Subscription::connect(client, base, interval, layout, ACCEPTED_WIRE_NAMES)
        .await
        .map_err(ConnectError::from)
}

#[cfg(test)]
/// `metriken_recorder::pump` under [`ACCEPTED_WIRE_NAMES`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn pump(
    idx: usize,
    sub: Subscription,
    client: reqwest::Client,
    base: reqwest::Url,
    interval: Duration,
    layout: Layout,
    timeout: Duration,
    tx: tokio::sync::mpsc::Sender<(usize, StreamEvent)>,
) {
    let mut cfg = metriken_recorder::PumpConfig::new(interval, timeout);
    cfg.layout = layout;
    cfg.wire_names = ACCEPTED_WIRE_NAMES.to_vec();
    metriken_recorder::pump(idx, sub, client, base, cfg, tx).await
}
