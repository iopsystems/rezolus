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
    /// Recording labels for `.rez` output (`--label k=v`); `source`/`host` auto-added.
    pub labels: Vec<(String, String)>,
    pub endpoints: Vec<EndpointConfig>,
    /// When set, record only while this command runs (perf-record style).
    pub command: Option<Vec<String>>,
    /// True when the format came from the default rather than from `--format`
    /// or an output extension. Only such a run may fall back from `.rez` to
    /// parquet for an endpoint `.rez` cannot record.
    pub format_defaulted: bool,
}

/// Default endpoint used when neither `--url` nor a positional URL is given.
const DEFAULT_URL: &str = "http://localhost:4241";

/// Default output file for a format, used when neither `-o` nor a positional
/// OUTPUT is given. The stem is constant; the extension follows the format so
/// the file never lies about what is inside it.
fn default_output_for(format: Format) -> PathBuf {
    PathBuf::from(match format {
        Format::Rez => "rezolus.rez",
        Format::Parquet => "rezolus.parquet",
        Format::Raw => "rezolus.raw",
    })
}

/// The format an output path's extension names, if it names one. An unknown
/// extension has no opinion, leaving `--format` (or the parquet fallback) to
/// decide.
fn format_from_extension(path: &Path) -> Option<Format> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rez") => Some(Format::Rez),
        Some("parquet") => Some(Format::Parquet),
        Some("raw") => Some(Format::Raw),
        _ => None,
    }
}

/// A resolved `(format, output)` pair.
#[derive(Debug)]
pub struct OutputPlan {
    pub format: Format,
    pub output: PathBuf,
    /// True only when nothing pinned the format: no `--format`, and no output
    /// path to read an extension off. Those runs default to `.rez`, and only
    /// those may be demoted to parquet when the endpoint cannot be recorded
    /// into a `.rez` (see `recorder::run`).
    pub defaulted: bool,
}

/// Resolve the output format and path.
///
/// `--format` and the output extension are both authoritative, so when both
/// name a format and disagree the command is rejected rather than silently
/// resolved one way: writing a `.rez` archive to a file called `out.parquet`
/// (which is what the old extension-wins rule did) breaks every tool
/// downstream that dispatches on the name.
fn resolve_format_and_output(
    explicit_format: Option<Format>,
    output: Option<&Path>,
) -> Result<OutputPlan, String> {
    let Some(path) = output else {
        let format = explicit_format.unwrap_or(Format::Rez);
        return Ok(OutputPlan {
            format,
            output: default_output_for(format),
            defaulted: explicit_format.is_none(),
        });
    };

    let from_ext = format_from_extension(path);
    if let (Some(flag), Some(ext)) = (explicit_format, from_ext) {
        if flag != ext {
            return Err(format!(
                "--format {} conflicts with the output path {}; drop one of them",
                format_name(flag),
                path.display()
            ));
        }
    }

    Ok(OutputPlan {
        format: explicit_format.or(from_ext).unwrap_or(Format::Parquet),
        output: path.to_path_buf(),
        defaulted: false,
    })
}

pub fn format_name(format: Format) -> &'static str {
    match format {
        Format::Rez => "rez",
        Format::Parquet => "parquet",
        Format::Raw => "raw",
    }
}

/// Resolve the recording URL from the `--url` flag and the deprecated
/// positional URL. Returns `(url, positional_was_used)`. Errors if both are
/// supplied.
fn resolve_url(flag: Option<&Url>, positional: Option<&Url>) -> Result<(Url, bool), String> {
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
/// OUTPUT. Returns `(path, positional_was_used)`; `None` means neither was
/// given, which is what lets the format pick the default filename. Errors if
/// both are supplied.
fn resolve_output(
    flag: Option<&Path>,
    positional: Option<&Path>,
) -> Result<(Option<PathBuf>, bool), String> {
    match (flag, positional) {
        (Some(_), Some(_)) => {
            Err("specify either -o/--output or the positional OUTPUT, not both".to_string())
        }
        (Some(p), None) => Ok((Some(p.to_path_buf()), false)),
        (None, Some(p)) => Ok((Some(p.to_path_buf()), true)),
        (None, None) => Ok((None, false)),
    }
}

impl RecordingConfig {
    pub fn from_args(args: &ArgMatches) -> Result<Self, String> {
        let verbose = *args.get_one::<u8>("VERBOSE").unwrap_or(&0);
        let interval = *args
            .get_one::<humantime::Duration>("INTERVAL")
            .unwrap_or(&humantime::Duration::from_str("1s").unwrap());
        let duration = args.get_one::<humantime::Duration>("DURATION").copied();
        let explicit_format = args.get_one::<Format>("FORMAT").copied();
        let separate = args.get_flag("SEPARATE");
        let metadata: Vec<(String, String)> = args
            .get_many::<String>("METADATA")
            .unwrap_or_default()
            .filter_map(|s| {
                s.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        let labels: Vec<(String, String)> = args
            .get_many::<String>("LABEL")
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
        let (explicit_output, output_deprecated) = resolve_output(out_flag, out_pos)?;
        let plan = resolve_format_and_output(explicit_format, explicit_output.as_deref())?;

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

            // The TOML `[recording]` table stands in for the flags it
            // mirrors: its `format` is as explicit as `--format`, and its
            // `output` is mandatory, so a config-file run is never a
            // defaulted-format run.
            let config_format = match explicit_format {
                Some(f) => Some(f),
                None => match toml_cfg.recording.format {
                    Some(ref s) => Some(match s.as_str() {
                        "parquet" => Format::Parquet,
                        "raw" => Format::Raw,
                        "rez" => Format::Rez,
                        other => return Err(format!("unknown format in config: {other}")),
                    }),
                    None => None,
                },
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

            let config_output = match explicit_output {
                Some(ref p) => p.clone(),
                None => PathBuf::from(toml_cfg.recording.output),
            };
            let plan = resolve_format_and_output(config_format, Some(&config_output))?;

            return Ok(RecordingConfig {
                interval,
                duration,
                format: plan.format,
                verbose,
                output: plan.output,
                separate,
                metadata,
                labels,
                endpoints: toml_cfg.endpoints,
                command: command.clone(),
                format_defaulted: plan.defaulted,
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
                format: plan.format,
                verbose,
                output: plan.output,
                separate,
                metadata,
                labels,
                endpoints,
                command: command.clone(),
                format_defaulted: plan.defaulted,
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
            format: plan.format,
            verbose,
            output: plan.output,
            separate,
            metadata,
            labels,
            endpoints: vec![endpoint],
            command,
            format_defaulted: plan.defaulted,
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
    fn label_flag_parses_into_config() {
        let matches = crate::recorder::command()
            .try_get_matches_from([
                "record",
                "--url",
                "http://localhost:4241",
                "--label",
                "arm=redis",
                "--label",
                "role=server",
            ])
            .unwrap();
        let cfg = RecordingConfig::from_args(&matches).unwrap();
        assert_eq!(
            cfg.labels,
            vec![
                ("arm".to_string(), "redis".to_string()),
                ("role".to_string(), "server".to_string()),
            ]
        );
    }

    /// `--rez-version` is gone, and a script still passing it must FAIL rather
    /// than be silently ignored.
    ///
    /// The flag selected a container this binary no longer writes. Accepting
    /// and ignoring it would be the worst outcome: someone asking for the tar
    /// container would get a v3 archive and no indication their request was
    /// dropped. Old archives are still readable, and `parquet upgrade`
    /// converts them.
    #[test]
    fn rez_version_is_rejected_now_that_only_one_container_is_written() {
        let parse = |args: &[&str]| crate::recorder::command().try_get_matches_from(args);

        // The recorder still works without it.
        assert!(parse(&["record", "--url", "http://localhost:4241"]).is_ok());

        for v in ["2", "3"] {
            assert!(
                parse(&[
                    "record",
                    "--url",
                    "http://localhost:4241",
                    "--rez-version",
                    v
                ])
                .is_err(),
                "--rez-version {v} must be rejected outright, not accepted and ignored"
            );
        }
    }

    /// `--node` / `--instance` are gone, and a script still passing them must
    /// FAIL rather than be silently ignored.
    ///
    /// They were added by #795 ("labeling at capture time") but no reader was
    /// ever written for either, in that commit or any since — so they spent
    /// their whole life accepting a value and dropping it. The keys they were
    /// meant to set are plain file metadata (`node`, `instance`, the same ones
    /// `recording combine` reads), so `--metadata node=web-01` was always the
    /// working form. Erroring is the point: someone who typed `--node` was
    /// already not getting what they asked for, and silence is how that went
    /// unnoticed for four months.
    #[test]
    fn node_and_instance_are_rejected_now_that_the_dead_flags_are_gone() {
        let parse = |args: &[&str]| crate::recorder::command().try_get_matches_from(args);

        assert!(parse(&["record", "--url", "http://localhost:4241"]).is_ok());

        for flag in ["--node", "--instance"] {
            assert!(
                parse(&["record", "--url", "http://localhost:4241", flag, "web-01"]).is_err(),
                "{flag} must be rejected outright, not accepted and ignored"
            );
        }

        // The replacement still works and lands as a metadata pair.
        let matches = parse(&[
            "record",
            "--url",
            "http://localhost:4241",
            "--metadata",
            "node=web-01",
        ])
        .unwrap();
        let cfg = RecordingConfig::from_args(&matches).unwrap();
        assert_eq!(
            cfg.metadata,
            vec![("node".to_string(), "web-01".to_string())]
        );
    }

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
    fn resolve_output_has_no_path_of_its_own_until_the_format_is_known() {
        let (out, dep) = resolve_output(None, None).unwrap();
        assert_eq!(out, None);
        assert!(!dep);
    }

    /// A bare `rezolus record` writes a `.rez` archive, and marks the run as
    /// defaulted so the recorder may fall back to parquet if the endpoint
    /// turns out to be one `.rez` cannot record.
    #[test]
    fn no_output_and_no_format_defaults_to_rez() {
        let plan = resolve_format_and_output(None, None).unwrap();
        assert_eq!(plan.format, Format::Rez);
        assert_eq!(plan.output, PathBuf::from("rezolus.rez"));
        assert!(plan.defaulted);
    }

    /// `--format X` with no `-o` names the file after the format, so the
    /// extension never lies about the contents. `--format raw` used to write
    /// its msgpack stream to a file called `rezolus.parquet`.
    #[test]
    fn an_explicit_format_names_the_default_file_after_itself() {
        for (format, name) in [
            (Format::Rez, "rezolus.rez"),
            (Format::Parquet, "rezolus.parquet"),
            (Format::Raw, "rezolus.raw"),
        ] {
            let plan = resolve_format_and_output(Some(format), None).unwrap();
            assert_eq!(plan.format, format);
            assert_eq!(plan.output, PathBuf::from(name));
            assert!(
                !plan.defaulted,
                "{name}: an explicit --format is a choice, not a default"
            );
        }
    }

    #[test]
    fn the_output_extension_picks_the_format() {
        for (name, format) in [
            ("out.rez", Format::Rez),
            ("out.parquet", Format::Parquet),
            ("out.raw", Format::Raw),
        ] {
            let plan = resolve_format_and_output(None, Some(Path::new(name))).unwrap();
            assert_eq!(plan.format, format, "{name}");
            assert_eq!(plan.output, PathBuf::from(name));
            assert!(!plan.defaulted, "{name}: the path is a choice too");
        }
    }

    /// An extension nobody recognizes is not an opinion, so parquet stays the
    /// fallback for a named path — `.rez` is only ever chosen by a name that
    /// says `.rez`, or by having nothing to go on at all.
    #[test]
    fn an_unknown_extension_leaves_the_format_to_the_flag() {
        let plan = resolve_format_and_output(None, Some(Path::new("out.dat"))).unwrap();
        assert_eq!(plan.format, Format::Parquet);
        assert!(!plan.defaulted);

        let plan =
            resolve_format_and_output(Some(Format::Rez), Some(Path::new("out.dat"))).unwrap();
        assert_eq!(plan.format, Format::Rez);
        assert_eq!(plan.output, PathBuf::from("out.dat"));
    }

    /// Silently resolving this either way produces a file whose name lies:
    /// a `.rez` archive called `out.parquet`, or a parquet table called
    /// `out.rez`. Everything downstream dispatches on that name.
    #[test]
    fn a_format_contradicting_the_extension_is_rejected() {
        let err = resolve_format_and_output(Some(Format::Parquet), Some(Path::new("out.rez")))
            .expect_err("--format parquet with a .rez path must not be resolved silently");
        assert!(err.contains("conflicts"), "{err}");

        let err = resolve_format_and_output(Some(Format::Rez), Some(Path::new("out.parquet")))
            .expect_err("--format rez with a .parquet path must not be resolved silently");
        assert!(err.contains("conflicts"), "{err}");

        // Agreeing is fine, and stays fine.
        let plan =
            resolve_format_and_output(Some(Format::Rez), Some(Path::new("out.rez"))).unwrap();
        assert_eq!(plan.format, Format::Rez);
    }

    /// A config-file run always has an output (the TOML field is mandatory),
    /// so it is never a defaulted-format run and never falls back.
    #[test]
    fn a_config_file_run_is_never_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        std::fs::write(
            &path,
            "[recording]\noutput = \"run.parquet\"\n\n[[endpoints]]\nurl = \"http://localhost:4241\"\n",
        )
        .unwrap();

        let matches = crate::recorder::command()
            .try_get_matches_from(["record", "--config", path.to_str().unwrap()])
            .unwrap();
        let cfg = RecordingConfig::from_args(&matches).unwrap();
        assert_eq!(cfg.format, Format::Parquet);
        assert_eq!(cfg.output, PathBuf::from("run.parquet"));
        assert!(!cfg.format_defaulted);
    }

    #[test]
    fn resolve_output_positional_is_deprecated() {
        let pos = PathBuf::from("legacy.parquet");
        let (out, dep) = resolve_output(None, Some(&pos)).unwrap();
        assert_eq!(out, Some(pos));
        assert!(dep);
    }

    #[test]
    fn resolve_output_both_is_error() {
        let flag = PathBuf::from("new.parquet");
        let pos = PathBuf::from("legacy.parquet");
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
