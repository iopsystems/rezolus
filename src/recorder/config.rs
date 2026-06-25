use super::endpoint::{EndpointConfig, Protocol};
use crate::Format;

use clap::ArgMatches;
use reqwest::Url;
use serde::Deserialize;

use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Deserialize)]
struct TomlConfig {
    recording: RecordingSection,
    #[serde(default)]
    endpoints: Vec<EndpointConfig>,
    #[serde(default)]
    gpu_amd_pmu: Option<PmuSection>,
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

/// `[gpu_amd_pmu]` section: on-demand AMD GPU hardware counter recording.
/// Mirrors how the agent's `gpu_amd_pmu` sampler is configured.
#[derive(Debug, Deserialize)]
struct PmuSection {
    /// Enable PMU recording. Defaults to true when the section is present.
    #[serde(default = "default_true")]
    enabled: bool,
    /// GPU indices to record. Omit to record every detected GPU.
    #[serde(default)]
    gpus: Option<Vec<usize>>,
    /// PMU event (counter) names. Omit to use the sampler's default set.
    #[serde(default)]
    events: Vec<String>,
    /// AMD GPU performance level to set before recording. Omit to leave the GPU
    /// power state untouched.
    #[serde(default)]
    gpu_perf_level: Option<String>,
}

fn default_true() -> bool {
    true
}

/// On-demand AMD GPU PMU recording settings, resolved from CLI flags and/or the
/// config file's `[gpu_amd_pmu]` section. The fields are only read on Linux,
/// where PMU recording is supported.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Debug, Default)]
pub struct PmuRecordingConfig {
    pub gpus: Option<Vec<usize>>,
    pub events: Vec<String>,
    /// AMD GPU performance level to set before recording (`None` = leave as-is).
    pub gpu_perf_level: Option<String>,
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
    /// On-demand AMD GPU PMU recording, if requested.
    pub pmu: Option<PmuRecordingConfig>,
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

        // AMD GPU PMU recording, requested via CLI flags. The TOML
        // `[gpu_amd_pmu]` section (config mode) can also enable it; CLI flags,
        // when present, take precedence and turn it on regardless of mode.
        let pmu_enabled_cli = args.get_flag("GPU_AMD_PMU");
        let pmu_gpus_cli = args
            .get_many::<usize>("GPU_AMD_PMU_GPUS")
            .map(|v| v.copied().collect::<Vec<usize>>());
        let pmu_events_cli = args
            .get_many::<String>("GPU_AMD_PMU_EVENTS")
            .map(|v| v.cloned().collect::<Vec<String>>());
        let pmu_perf_level_cli = args.get_one::<String>("GPU_PERF_LEVEL").cloned();
        // PMU is on if --gpu-amd-pmu was passed, or any of its sub-options were.
        let pmu_cli = if pmu_enabled_cli
            || pmu_gpus_cli.is_some()
            || pmu_events_cli.is_some()
            || pmu_perf_level_cli.is_some()
        {
            Some(PmuRecordingConfig {
                gpus: pmu_gpus_cli.clone(),
                events: pmu_events_cli.clone().unwrap_or_default(),
                gpu_perf_level: pmu_perf_level_cli.clone(),
            })
        } else {
            None
        };

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

            // Resolve PMU config: CLI flags win; otherwise the TOML section.
            let pmu = pmu_cli.clone().or_else(|| {
                toml_cfg.gpu_amd_pmu.as_ref().and_then(|s| {
                    if s.enabled {
                        Some(PmuRecordingConfig {
                            gpus: s.gpus.clone(),
                            events: s.events.clone(),
                            gpu_perf_level: s.gpu_perf_level.clone(),
                        })
                    } else {
                        None
                    }
                })
            });

            // At least one source is required: an HTTP endpoint or PMU.
            if toml_cfg.endpoints.is_empty() && pmu.is_none() {
                return Err(
                    "config file must define at least one endpoint or a [gpu_amd_pmu] section"
                        .to_string(),
                );
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
                pmu,
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
            let output = resolve_output(args)
                .ok_or_else(|| "OUTPUT is required when using --endpoint".to_string())?;

            return Ok(RecordingConfig {
                interval,
                duration,
                format,
                verbose,
                output,
                separate,
                metadata,
                endpoints,
                pmu: pmu_cli,
            });
        }

        // Mode 3: positional <URL> <OUTPUT> (backward compat).
        //
        // When no URL is given but PMU was requested via CLI flags, fall back to
        // a PMU-only recording (the positional argument is then the OUTPUT path,
        // or `--output` may be used).
        let url = args.get_one::<Url>("URL").cloned();

        if url.is_none() {
            if let Some(pmu) = pmu_cli.clone() {
                // PMU-only recording. The positional URL slot is URL-validated,
                // so the output path must come from `--output`/`-o` (or the
                // OUTPUT positional, which is only reachable when a URL is also
                // given). Require `--output` here.
                let output = resolve_output(args)
                    .ok_or_else(|| "--output is required for PMU-only recording".to_string())?;
                return Ok(RecordingConfig {
                    interval,
                    duration,
                    format,
                    verbose,
                    output,
                    separate,
                    metadata,
                    endpoints: Vec::new(),
                    pmu: Some(pmu),
                });
            }
        }

        let url = url.ok_or_else(|| {
            "must specify one of: <URL> <OUTPUT>, --endpoint, --config, or --gpu-amd-pmu"
                .to_string()
        })?;
        let output = resolve_output(args).ok_or_else(|| "OUTPUT is required".to_string())?;

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
            output,
            separate,
            metadata,
            endpoints: vec![endpoint],
            pmu: pmu_cli,
        })
    }
}

/// Resolve the output path from `--output`/`-o` (preferred) or the OUTPUT
/// positional argument. Returns `None` when neither is set.
fn resolve_output(args: &ArgMatches) -> Option<PathBuf> {
    args.get_one::<PathBuf>("OUTPUT_FLAG")
        .or_else(|| args.get_one::<PathBuf>("OUTPUT"))
        .cloned()
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

    // ----- PMU configuration -----

    fn matches(argv: &[&str]) -> clap::ArgMatches {
        let mut full = vec!["record"];
        full.extend_from_slice(argv);
        crate::recorder::command().get_matches_from(full)
    }

    #[test]
    fn test_pmu_cli_flag_only() {
        // `--gpu-amd-pmu -o <OUTPUT>` is a PMU-only recording.
        let m = matches(&["--gpu-amd-pmu", "-o", "/tmp/out.parquet"]);
        let cfg = RecordingConfig::from_args(&m).unwrap();
        assert!(cfg.endpoints.is_empty());
        let pmu = cfg.pmu.expect("pmu config");
        assert!(pmu.gpus.is_none());
        assert!(pmu.events.is_empty());
        assert_eq!(cfg.output, PathBuf::from("/tmp/out.parquet"));
    }

    #[test]
    fn test_pmu_only_requires_output() {
        // PMU-only with no output path is an error.
        let m = matches(&["--gpu-amd-pmu"]);
        assert!(RecordingConfig::from_args(&m).is_err());
    }

    #[test]
    fn test_pmu_cli_gpus_and_events() {
        let m = matches(&[
            "--gpu-amd-pmu-gpus",
            "0,2",
            "--gpu-amd-pmu-events",
            "SQ_WAVES,GRBM_COUNT",
            "-o",
            "/tmp/out.parquet",
        ]);
        let cfg = RecordingConfig::from_args(&m).unwrap();
        let pmu = cfg.pmu.expect("pmu config");
        assert_eq!(pmu.gpus, Some(vec![0, 2]));
        assert_eq!(pmu.events, vec!["SQ_WAVES", "GRBM_COUNT"]);
        assert_eq!(pmu.gpu_perf_level, None);
    }

    #[test]
    fn test_pmu_cli_perf_level() {
        // --gpu-perf-level alone enables PMU recording and is captured.
        let m = matches(&["--gpu-perf-level", "stable_std", "-o", "/tmp/out.parquet"]);
        let cfg = RecordingConfig::from_args(&m).unwrap();
        let pmu = cfg.pmu.expect("pmu config");
        assert_eq!(pmu.gpu_perf_level.as_deref(), Some("stable_std"));
    }

    #[test]
    fn test_pmu_cli_perf_level_rejects_invalid() {
        // clap's value_parser restricts the allowed set, so this fails at parse.
        let mut full = vec![
            "record",
            "--gpu-perf-level",
            "turbo",
            "-o",
            "/tmp/out.parquet",
        ];
        let res = crate::recorder::command().try_get_matches_from(std::mem::take(&mut full));
        assert!(res.is_err());
    }

    #[test]
    fn test_pmu_toml_perf_level() {
        let toml = r#"
            [recording]
            output = "/tmp/out.parquet"

            [gpu_amd_pmu]
            gpu_perf_level = "stable_peak"
        "#;
        let cfg: TomlConfig = toml::from_str(toml).unwrap();
        let pmu = cfg.gpu_amd_pmu.unwrap();
        assert_eq!(pmu.gpu_perf_level.as_deref(), Some("stable_peak"));
    }

    #[test]
    fn test_pmu_with_positional_url_endpoint() {
        // PMU can be combined with a positional URL endpoint.
        let m = matches(&["--gpu-amd-pmu", "http://localhost:4241", "/tmp/out.parquet"]);
        let cfg = RecordingConfig::from_args(&m).unwrap();
        assert_eq!(cfg.endpoints.len(), 1);
        assert!(cfg.pmu.is_some());
    }

    #[test]
    fn test_no_source_errors() {
        // No URL, no endpoint, no PMU -> error.
        let m = matches(&[]);
        assert!(RecordingConfig::from_args(&m).is_err());
    }

    #[test]
    fn test_pmu_toml_section_defaults() {
        let toml = r#"
            [recording]
            output = "/tmp/out.parquet"

            [gpu_amd_pmu]
        "#;
        let cfg: TomlConfig = toml::from_str(toml).unwrap();
        let pmu = cfg.gpu_amd_pmu.expect("section present");
        assert!(pmu.enabled, "enabled defaults to true when present");
        assert!(pmu.gpus.is_none());
        assert!(pmu.events.is_empty());
    }

    #[test]
    fn test_pmu_toml_section_full() {
        let toml = r#"
            [recording]
            output = "/tmp/out.parquet"

            [gpu_amd_pmu]
            enabled = true
            gpus = [0, 1]
            events = ["SQ_WAVES", "GL2C_HIT"]
        "#;
        let cfg: TomlConfig = toml::from_str(toml).unwrap();
        let pmu = cfg.gpu_amd_pmu.unwrap();
        assert_eq!(pmu.gpus, Some(vec![0, 1]));
        assert_eq!(pmu.events, vec!["SQ_WAVES", "GL2C_HIT"]);
    }

    #[test]
    fn test_pmu_only_toml_no_endpoints_ok() {
        // A config with only a [gpu_amd_pmu] section (no endpoints) is valid.
        let dir = std::env::temp_dir();
        let path = dir.join("rezolus_test_pmu_only.toml");
        std::fs::write(
            &path,
            "[recording]\noutput = \"/tmp/out.parquet\"\n\n[gpu_amd_pmu]\ngpus = [0]\n",
        )
        .unwrap();
        let m = matches(&["--config", path.to_str().unwrap()]);
        let cfg = RecordingConfig::from_args(&m).unwrap();
        assert!(cfg.endpoints.is_empty());
        let pmu = cfg.pmu.expect("pmu config");
        assert_eq!(pmu.gpus, Some(vec![0]));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_disabled_pmu_toml_with_no_endpoints_errors() {
        let dir = std::env::temp_dir();
        let path = dir.join("rezolus_test_pmu_disabled.toml");
        std::fs::write(
            &path,
            "[recording]\noutput = \"/tmp/out.parquet\"\n\n[gpu_amd_pmu]\nenabled = false\n",
        )
        .unwrap();
        let m = matches(&["--config", path.to_str().unwrap()]);
        // Disabled PMU + no endpoints -> error.
        assert!(RecordingConfig::from_args(&m).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
