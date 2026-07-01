use super::endpoint::{EndpointConfig, Protocol};
use crate::Format;

use clap::ArgMatches;
use reqwest::Url;
use serde::Deserialize;

use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

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

pub struct RecordingConfig {
    pub interval: humantime::Duration,
    pub duration: Option<humantime::Duration>,
    pub format: Format,
    pub verbose: u8,
    pub output: PathBuf,
    pub separate: bool,
    pub metadata: Vec<(String, String)>,
    pub endpoints: Vec<EndpointConfig>,
    /// When set, record only while this command runs (perf-record style).
    pub command: Option<Vec<String>>,
}

/// Default endpoint used when neither `--url` nor a positional URL is given.
const DEFAULT_URL: &str = "http://localhost:4241";
/// Default output file used when neither `-o` nor a positional OUTPUT is given.
const DEFAULT_OUTPUT: &str = "rezolus.parquet";

/// Resolve the recording URL from the `--url` flag and the deprecated
/// positional URL. Returns `(url, positional_was_used)`. Errors if both are
/// supplied.
pub fn resolve_url(flag: Option<&Url>, positional: Option<&Url>) -> Result<(Url, bool), String> {
    match (flag, positional) {
        (Some(_), Some(_)) => {
            Err("specify either --url or the positional URL, not both".to_string())
        }
        (Some(u), None) => Ok((u.clone(), false)),
        (None, Some(u)) => Ok((u.clone(), true)),
        (None, None) => Ok((Url::parse(DEFAULT_URL).unwrap(), false)),
    }
}

/// Resolve the output path from `-o/--output` and the deprecated positional
/// OUTPUT. Returns `(path, positional_was_used)`. Errors if both are supplied.
pub fn resolve_output(
    flag: Option<&Path>,
    positional: Option<&Path>,
) -> Result<(PathBuf, bool), String> {
    match (flag, positional) {
        (Some(_), Some(_)) => {
            Err("specify either -o/--output or the positional OUTPUT, not both".to_string())
        }
        (Some(p), None) => Ok((p.to_path_buf(), false)),
        (None, Some(p)) => Ok((p.to_path_buf(), true)),
        (None, None) => Ok((PathBuf::from(DEFAULT_OUTPUT), false)),
    }
}

impl RecordingConfig {
    pub fn from_args(args: &ArgMatches) -> Result<Self, String> {
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

        let command: Option<Vec<String>> = args
            .get_many::<String>("COMMAND")
            .map(|vals| vals.map(|s| s.to_string()).collect());

        // Resolve output once (used by every mode except --config, which
        // prefers its TOML output when -o is not given).
        let out_flag = args.get_one::<PathBuf>("OUTPUT_FLAG").map(|p| p.as_path());
        let out_pos = args.get_one::<PathBuf>("OUTPUT").map(|p| p.as_path());
        let (resolved_output, output_deprecated) = resolve_output(out_flag, out_pos)?;

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

            let output = if args.get_one::<PathBuf>("OUTPUT_FLAG").is_some() {
                resolved_output.clone()
            } else {
                PathBuf::from(toml_cfg.recording.output)
            };

            return Ok(RecordingConfig {
                interval,
                duration,
                format,
                verbose,
                output,
                separate,
                metadata,
                endpoints: toml_cfg.endpoints,
                command: command.clone(),
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

            return Ok(RecordingConfig {
                interval,
                duration,
                format,
                verbose,
                output: resolved_output.clone(),
                separate,
                metadata,
                endpoints,
                command: command.clone(),
            });
        }

        // Mode 3: --url / positional <URL>, single endpoint.
        let url_flag = args.get_one::<Url>("URL_FLAG");
        let url_pos = args.get_one::<Url>("URL");
        let (url, url_deprecated) = resolve_url(url_flag, url_pos)?;

        if url_deprecated {
            eprintln!("note: the positional URL is deprecated, use --url");
        }
        if output_deprecated {
            eprintln!("note: the positional OUTPUT is deprecated, use -o/--output");
        }

        let source = metadata
            .iter()
            .find(|(k, _)| k == "source")
            .map(|(_, v)| v.clone());

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
            output: resolved_output,
            separate,
            metadata,
            endpoints: vec![endpoint],
            command,
        })
    }
}

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

    Ok(EndpointConfig {
        url,
        source,
        role,
        protocol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::endpoint::Protocol;

    #[test]
    fn resolve_url_defaults_to_localhost() {
        let (url, deprecated) = resolve_url(None, None).unwrap();
        assert_eq!(url.as_str(), "http://localhost:4241/");
        assert!(!deprecated);
    }

    #[test]
    fn resolve_url_flag_wins() {
        let flag = Url::parse("http://example:9090").unwrap();
        let (url, deprecated) = resolve_url(Some(&flag), None).unwrap();
        assert_eq!(url.as_str(), "http://example:9090/");
        assert!(!deprecated);
    }

    #[test]
    fn resolve_url_positional_is_deprecated() {
        let pos = Url::parse("http://host:4241").unwrap();
        let (url, deprecated) = resolve_url(None, Some(&pos)).unwrap();
        assert_eq!(url.as_str(), "http://host:4241/");
        assert!(deprecated);
    }

    #[test]
    fn resolve_url_both_is_error() {
        let a = Url::parse("http://a:1").unwrap();
        let b = Url::parse("http://b:2").unwrap();
        assert!(resolve_url(Some(&a), Some(&b)).is_err());
    }

    #[test]
    fn resolve_output_default_and_deprecation() {
        let (out, dep) = resolve_output(None, None).unwrap();
        assert_eq!(out, PathBuf::from("rezolus.parquet"));
        assert!(!dep);

        let pos = PathBuf::from("legacy.parquet");
        let (out, dep) = resolve_output(None, Some(&pos)).unwrap();
        assert_eq!(out, pos);
        assert!(dep);

        let flag = PathBuf::from("new.parquet");
        assert!(resolve_output(Some(&flag), Some(&pos)).is_err());
    }

    #[test]
    fn test_parse_endpoint_str_full() {
        let ep = parse_endpoint_str("http://localhost:4241,source=rezolus,role=agent").unwrap();
        assert_eq!(ep.source.as_deref(), Some("rezolus"));
        assert_eq!(ep.role.as_deref(), Some("agent"));
    }

    #[test]
    fn test_parse_endpoint_str_url_only() {
        let ep = parse_endpoint_str("http://localhost:9090/metrics").unwrap();
        assert!(ep.source.is_none()); // probe resolves it
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
