use super::endpoint::{infer_source_name, EndpointConfig, Protocol};
use crate::Format;

use clap::ArgMatches;
use reqwest::Url;
use serde::Deserialize;

use std::path::PathBuf;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// TOML config file structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TomlConfig {
    recording: RecordingSection,
    endpoints: Vec<EndpointConfig>,
}

#[derive(Debug, Deserialize)]
struct RecordingSection {
    interval: Option<String>,
    output: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    separate: Option<bool>,
}

// ---------------------------------------------------------------------------
// Unified config used by run()
// ---------------------------------------------------------------------------

pub struct RecordingConfig {
    pub interval: humantime::Duration,
    pub duration: Option<humantime::Duration>,
    pub format: Format,
    pub verbose: u8,
    pub output: PathBuf,
    pub separate: bool,
    pub metadata: Vec<(String, String)>,
    pub endpoints: Vec<EndpointConfig>,
}

impl RecordingConfig {
    pub fn from_args(args: &ArgMatches) -> Result<Self, String> {
        // Common CLI values shared across all modes
        let verbose = *args.get_one::<u8>("VERBOSE").unwrap_or(&0);
        let interval = *args
            .get_one::<humantime::Duration>("INTERVAL")
            .unwrap_or(&humantime::Duration::from_str("1s").unwrap());
        let duration = args.get_one::<humantime::Duration>("DURATION").copied();
        let format = args
            .get_one::<Format>("FORMAT")
            .copied()
            .unwrap_or(Format::Parquet);
        let separate = args.get_flag("SEPARATE");
        let metadata: Vec<(String, String)> = args
            .get_many::<String>("METADATA")
            .unwrap_or_default()
            .filter_map(|s| {
                s.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();

        // Mode 1: --config file.toml
        if let Some(config_path) = args.get_one::<PathBuf>("CONFIG_FILE") {
            let contents = std::fs::read_to_string(config_path)
                .map_err(|e| format!("failed to read config file: {e}"))?;
            let toml_cfg: TomlConfig =
                toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))?;

            // CLI flags override TOML values
            let interval =
                if args.value_source("INTERVAL") == Some(clap::parser::ValueSource::CommandLine) {
                    interval
                } else if let Some(ref s) = toml_cfg.recording.interval {
                    humantime::Duration::from_str(s)
                        .map_err(|e| format!("invalid interval in config: {e}"))?
                } else {
                    interval
                };

            let format =
                if args.value_source("FORMAT") == Some(clap::parser::ValueSource::CommandLine) {
                    format
                } else if let Some(ref s) = toml_cfg.recording.format {
                    match s.as_str() {
                        "parquet" => Format::Parquet,
                        "raw" => Format::Raw,
                        other => return Err(format!("unknown format in config: {other}")),
                    }
                } else {
                    format
                };

            let separate =
                if args.value_source("SEPARATE") == Some(clap::parser::ValueSource::CommandLine) {
                    separate
                } else {
                    toml_cfg.recording.separate.unwrap_or(false)
                };

            if toml_cfg.endpoints.is_empty() {
                return Err("config file must define at least one endpoint".to_string());
            }

            return Ok(RecordingConfig {
                interval,
                duration,
                format,
                verbose,
                output: PathBuf::from(toml_cfg.recording.output),
                separate,
                metadata,
                endpoints: toml_cfg.endpoints,
            });
        }

        // Mode 2: --endpoint url,source=name (repeatable)
        if let Some(endpoint_strs) = args.get_many::<String>("ENDPOINT") {
            let endpoints: Result<Vec<EndpointConfig>, String> =
                endpoint_strs.map(|s| parse_endpoint_str(s)).collect();
            let endpoints = endpoints?;

            if endpoints.is_empty() {
                return Err("at least one --endpoint is required".to_string());
            }

            // OUTPUT is required for --endpoint mode
            let output = args
                .get_one::<PathBuf>("OUTPUT")
                .ok_or_else(|| "OUTPUT is required when using --endpoint".to_string())?
                .to_path_buf();

            return Ok(RecordingConfig {
                interval,
                duration,
                format,
                verbose,
                output,
                separate,
                metadata,
                endpoints,
            });
        }

        // Mode 3: positional <URL> <OUTPUT> (backward compat)
        let url = args
            .get_one::<Url>("URL")
            .ok_or_else(|| {
                "must specify one of: <URL> <OUTPUT>, --endpoint, or --config".to_string()
            })?
            .clone();
        let output = args
            .get_one::<PathBuf>("OUTPUT")
            .ok_or_else(|| "OUTPUT is required".to_string())?
            .to_path_buf();

        // Extract source from --metadata source=xxx, else infer
        let source = metadata
            .iter()
            .find(|(k, _)| k == "source")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| infer_source_name(&url));

        let endpoint = EndpointConfig {
            url,
            source,
            role: None,
            protocol: None,
        };

        Ok(RecordingConfig {
            interval,
            duration,
            format,
            verbose,
            output,
            separate,
            metadata,
            endpoints: vec![endpoint],
        })
    }
}

// ---------------------------------------------------------------------------
// --endpoint string parser
// ---------------------------------------------------------------------------

/// Parse `"http://host:port/path,source=name,role=agent,protocol=prometheus"`.
/// URL is everything before the first comma. Key=value pairs after.
pub fn parse_endpoint_str(s: &str) -> Result<EndpointConfig, String> {
    let (url_str, rest) = match s.find(',') {
        Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
        None => (s, None),
    };

    let url = Url::parse(url_str).map_err(|e| format!("invalid URL '{url_str}': {e}"))?;

    let mut source: Option<String> = None;
    let mut role: Option<String> = None;
    let mut protocol: Option<Protocol> = None;

    if let Some(opts) = rest {
        for pair in opts.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, got: '{pair}'"))?;
            match key {
                "source" => source = Some(value.to_string()),
                "role" => role = Some(value.to_string()),
                "protocol" => {
                    protocol = Some(match value {
                        "msgpack" => Protocol::Msgpack,
                        "prometheus" => Protocol::Prometheus,
                        other => return Err(format!("unknown protocol: '{other}'")),
                    });
                }
                other => return Err(format!("unknown endpoint option: '{other}'")),
            }
        }
    }

    let source = source.unwrap_or_else(|| infer_source_name(&url));

    Ok(EndpointConfig {
        url,
        source,
        role,
        protocol,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::endpoint::Protocol;

    #[test]
    fn test_parse_endpoint_str_full() {
        let ep = parse_endpoint_str("http://localhost:4241,source=rezolus,role=agent").unwrap();
        assert_eq!(ep.source, "rezolus");
        assert_eq!(ep.role.as_deref(), Some("agent"));
    }

    #[test]
    fn test_parse_endpoint_str_url_only() {
        let ep = parse_endpoint_str("http://localhost:9090/metrics").unwrap();
        assert_eq!(ep.source, "localhost-9090"); // inferred
        assert!(ep.role.is_none());
    }

    #[test]
    fn test_parse_endpoint_str_with_protocol() {
        let ep =
            parse_endpoint_str("http://host:9090/metrics,source=vllm,protocol=prometheus").unwrap();
        assert_eq!(ep.protocol, Some(Protocol::Prometheus));
    }

    #[test]
    fn test_parse_endpoint_str_invalid_url() {
        let result = parse_endpoint_str("not-a-url,source=test");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_endpoint_str_unknown_option() {
        let result = parse_endpoint_str("http://host:80,source=x,foo=bar");
        assert!(result.is_err());
    }
}
