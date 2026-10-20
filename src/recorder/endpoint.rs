use reqwest::Url;
use serde::Deserialize;
use std::path::PathBuf;

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
    /// `template=FILE`: a KPI template (a `ServiceExtension` JSON file)
    /// written into this endpoint's recording as `service_queries`. Relative
    /// to the working directory on `--endpoint`, and to the config file's
    /// directory in a `--config` file (resolved by `from_args`).
    #[serde(default)]
    pub template: Option<PathBuf>,
    /// The template's text, read and checked by
    /// [`EndpointConfig::load_template`] before anything is recorded.
    #[serde(skip)]
    pub template_json: Option<String>,
    /// The endpoint's A/B arm (`arm=`), a label on its recording: recordings
    /// that carry one open in the viewer as a comparison.
    #[serde(default)]
    pub arm: Option<String>,
}

impl EndpointConfig {
    /// Read and check `template` (see `parquet_tools::annotate::load_queries`)
    /// into `template_json`. An error names the endpoint, and the file
    /// through `load_queries`.
    pub fn load_template(&mut self) -> Result<(), String> {
        if let Some(path) = &self.template {
            let json = crate::parquet_tools::annotate::load_queries(path)
                .map_err(|e| format!("endpoint {}: template: {e}", self.url))?;
            self.template_json = Some(json);
        }
        Ok(())
    }
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
