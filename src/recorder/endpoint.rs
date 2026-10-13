use reqwest::Url;
use serde::Deserialize;

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
    pub fn transport(protocol: Option<&Protocol>) -> metriken_recorder::Transport {
        match protocol {
            None => metriken_recorder::Transport::Auto,
            Some(Protocol::Stream) => metriken_recorder::Transport::Stream,
            Some(Protocol::Prometheus) => metriken_recorder::Transport::Prometheus,
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
    /// agent, or a name inferred from the URL (`metriken-recorder`'s source
    /// identity).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub protocol: Option<Protocol>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_protocol_selects_the_transport() {
        use metriken_recorder::Transport;
        assert_eq!(Protocol::transport(None), Transport::Auto);
        assert_eq!(
            Protocol::transport(Some(&Protocol::Stream)),
            Transport::Stream
        );
        assert_eq!(
            Protocol::transport(Some(&Protocol::Prometheus)),
            Transport::Prometheus
        );
    }
}
