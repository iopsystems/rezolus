use super::endpoint::{EndpointConfig, Protocol};

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
    /// Kept so a config written for 5.x is refused by name: `dendro` is
    /// accepted, and every other value is refused by [`check_format`].
    #[serde(default)]
    format: Option<String>,
    /// As `format`: `separate = true` is refused (see [`refuse_separate`]).
    #[serde(default)]
    separate: Option<bool>,
}

pub struct RecordingConfig {
    pub interval: humantime::Duration,
    pub duration: Option<humantime::Duration>,
    pub verbose: u8,
    /// The `.dendro` archive to write. Always ends in `.dendro`: see
    /// [`check_output`].
    pub output: PathBuf,
    pub metadata: Vec<(String, String)>,
    /// Recording labels (`--label k=v`); `source`/`host` auto-added.
    pub labels: Vec<(String, String)>,
    pub endpoints: Vec<EndpointConfig>,
    /// When set, record only while this command runs (perf-record style).
    pub command: Option<Vec<String>>,
    /// `--record-command-line`: put the wrapped command's full argument list
    /// in the `run_start` event's `details`. Off by default because an
    /// argument list can carry paths and tokens the recording's reader should
    /// not see; the event's `description` is only the program name.
    pub record_command_line: bool,
}

/// Default endpoint used when neither `--url` nor a positional URL is given.
const DEFAULT_URL: &str = "http://localhost:4241";

/// Default output file, used when neither `-o` nor a positional OUTPUT is
/// given.
const DEFAULT_OUTPUT: &str = "rezolus.dendro";

/// The output formats 5.x wrote and 6.x does not, by the name `--format` and
/// the output extension used for each.
const REMOVED_FORMATS: [&str; 3] = ["rez", "parquet", "raw"];

/// Where an output path came from, so a refusal quotes it back the way the
/// caller wrote it.
#[derive(Clone, Copy)]
enum OutputOrigin<'a> {
    /// `-o/--output`.
    Flag,
    /// The deprecated positional OUTPUT.
    Positional,
    /// The `output` key of a `--config` file at this path.
    Config(&'a Path),
}

impl OutputOrigin<'_> {
    /// `path` as the caller wrote it: `-o out.rez`, `OUTPUT out.rez`, or
    /// `in rec.toml: output = "out.rez"`.
    fn quote(self, path: &Path) -> String {
        match self {
            Self::Flag => format!("-o {}", path.display()),
            Self::Positional => format!("OUTPUT {}", path.display()),
            Self::Config(file) => format!(
                "in {}: output = {:?}",
                file.display(),
                path.display().to_string()
            ),
        }
    }
}

/// "A rezolus 5.x recorder (5.27.0 or later)", the fallback every refusal of
/// a removed 5.x behaviour names, at the start of a sentence.
fn recorder_5x() -> String {
    format!("A rezolus 5.x recorder ({} or later)", super::RELEASE_5X)
}

/// The refusal for a format 6.x no longer writes. `asked` is the flag or
/// path that named it, quoted back first.
fn removed_format(asked: &str, format: &str) -> String {
    let convert = if format == "rez" {
        " `rezolus recording upgrade --to dendro in.rez -o out.dendro` converts a .rez \
         afterwards."
    } else {
        ""
    };
    format!(
        "{asked}: rezolus 6.x writes .dendro only (-o out.dendro); .{format} output was \
         removed in 6.0. {} still writes .{format}.{convert}",
        recorder_5x()
    )
}

/// Accept `--format dendro` (or the TOML `format` key), and refuse every
/// other value: the 5.x formats by name, anything else as unknown.
fn check_format(format: &str) -> Result<(), String> {
    match format {
        "dendro" => Ok(()),
        f if REMOVED_FORMATS.contains(&f) => Err(removed_format(&format!("--format {f}"), f)),
        other => Err(format!(
            "unknown format {other:?}: rezolus 6.x writes .dendro only"
        )),
    }
}

/// Accept an output path ending in `.dendro`, and refuse any other, quoting
/// the path back as `origin` gave it.
///
/// An extension 5.x read as a format (`.rez`, `.parquet`, `.raw`) is refused
/// by name. Any other extension, or none, is refused too: 5.x wrote parquet
/// there, so a caller passing `-o capture.dat` expects parquet, and a
/// `.dendro` archive under that name would be a file whose name does not say
/// what it holds.
fn check_output(path: &Path, origin: OutputOrigin) -> Result<(), String> {
    let asked = origin.quote(path);
    match path.extension().and_then(|e| e.to_str()) {
        Some("dendro") => Ok(()),
        Some(ext) if REMOVED_FORMATS.contains(&ext) => Err(removed_format(&asked, ext)),
        _ => Err(format!(
            "{asked}: the output must end in .dendro; rezolus 6.x writes .dendro archives \
             only. {} wrote this path as parquet and still does.",
            recorder_5x()
        )),
    }
}

/// `--separate` (or `separate = true`) wrote one parquet or raw file per
/// endpoint. A `.dendro` holds each endpoint as its own recording, so the
/// flag has nothing left to do. It is refused rather than ignored: a caller
/// expecting one file per endpoint would otherwise find one archive.
fn refuse_separate() -> String {
    format!(
        "--separate: rezolus 6.x writes one .dendro holding each endpoint as its own \
         recording, and --separate was removed in 6.0. {} still writes one parquet or \
         raw file per endpoint.",
        recorder_5x()
    )
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
/// given. Errors if both are supplied.
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
        if let Some(format) = args.get_one::<String>("FORMAT") {
            check_format(format)?;
        }
        if args.get_flag("SEPARATE") {
            return Err(refuse_separate());
        }
        let record_command_line = args.get_flag("RECORD_COMMAND_LINE");
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
        let explicit_origin = if output_deprecated {
            OutputOrigin::Positional
        } else {
            OutputOrigin::Flag
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

            if let Some(ref format) = toml_cfg.recording.format {
                check_format(format).map_err(|e| format!("in {}: {e}", config_path.display()))?;
            }
            if toml_cfg.recording.separate == Some(true) {
                return Err(format!(
                    "in {}: {}",
                    config_path.display(),
                    refuse_separate()
                ));
            }

            if toml_cfg.endpoints.is_empty() {
                return Err("config file must define at least one endpoint".to_string());
            }

            let (output, origin) = match explicit_output {
                Some(p) => (p, explicit_origin),
                None => (
                    PathBuf::from(toml_cfg.recording.output),
                    OutputOrigin::Config(config_path),
                ),
            };
            check_output(&output, origin)?;

            return Ok(RecordingConfig {
                interval,
                duration,
                verbose,
                output,
                metadata,
                labels,
                endpoints: toml_cfg.endpoints,
                command: command.clone(),
                record_command_line,
            });
        }

        let output = match explicit_output {
            Some(p) => {
                check_output(&p, explicit_origin)?;
                p
            }
            None => PathBuf::from(DEFAULT_OUTPUT),
        };

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
                verbose,
                output,
                metadata,
                labels,
                endpoints,
                command: command.clone(),
                record_command_line,
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
            verbose,
            output,
            metadata,
            labels,
            endpoints: vec![endpoint],
            command,
            record_command_line,
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

    fn parse(args: &[&str]) -> Result<RecordingConfig, String> {
        let mut argv = vec!["record"];
        argv.extend_from_slice(args);
        crate::recorder::command()
            .try_get_matches_from(argv)
            .map_err(|e| e.to_string())
            .and_then(|m| RecordingConfig::from_args(&m))
    }

    #[test]
    fn label_flag_parses_into_config() {
        let cfg = parse(&[
            "--url",
            "http://localhost:4241",
            "--label",
            "arm=redis",
            "--label",
            "role=server",
        ])
        .unwrap();
        assert_eq!(
            cfg.labels,
            vec![
                ("arm".to_string(), "redis".to_string()),
                ("role".to_string(), "server".to_string()),
            ]
        );
    }

    /// A bare `rezolus record` writes `rezolus.dendro`.
    #[test]
    fn no_output_defaults_to_rezolus_dendro() {
        assert_eq!(parse(&[]).unwrap().output, PathBuf::from("rezolus.dendro"));
        assert_eq!(
            parse(&["-o", "out.dendro"]).unwrap().output,
            PathBuf::from("out.dendro")
        );
    }

    /// Each extension 5.x read as a format is refused by name, pointing at
    /// `.dendro` and at a 5.x recorder, and quoting the path the way it was
    /// given: `-o out.rez`, or `OUTPUT out.rez` for the positional form.
    #[test]
    fn an_output_in_a_removed_format_is_refused() {
        for ext in ["rez", "parquet", "raw"] {
            let path = format!("out.{ext}");
            // `-o`, and the deprecated positional OUTPUT after a positional URL.
            for (args, asked) in [
                (vec!["-o", path.as_str()], format!("-o {path}: ")),
                (vec!["http://h:1", &path], format!("OUTPUT {path}: ")),
            ] {
                let err = parse(&args).err().unwrap_or_else(|| panic!("{args:?}"));
                assert!(err.starts_with(&asked), "{asked:?} starts {err}");
                for needle in [
                    "writes .dendro only",
                    "-o out.dendro",
                    &format!("5.x recorder ({} or later)", crate::recorder::RELEASE_5X),
                ] {
                    assert!(err.contains(needle), "{needle:?} in {err}");
                }
                assert_eq!(err.contains("upgrade --to dendro"), ext == "rez", "{err}");
            }
        }
    }

    /// Any other extension, or none, is refused too, pointing at the 5.x
    /// recorder that wrote parquet there.
    #[test]
    fn an_output_not_ending_in_dendro_is_refused() {
        for path in ["capture.dat", "capture", "out.dendro.tmp"] {
            let err = parse(&["-o", path])
                .err()
                .unwrap_or_else(|| panic!("{path}"));
            assert!(err.starts_with(&format!("-o {path}: ")), "{err}");
            assert!(err.contains("must end in .dendro"), "{err}");
            assert!(err.contains("5.x recorder"), "{err}");
            assert!(err.contains("as parquet"), "{err}");
        }
    }

    /// `--format dendro` is still accepted; the removed formats are refused
    /// by name and anything else as unknown.
    #[test]
    fn format_accepts_only_dendro() {
        assert!(parse(&["--format", "dendro"]).is_ok());
        assert!(parse(&["-f", "dendro", "-o", "x.dendro"]).is_ok());
        for f in ["rez", "parquet", "raw"] {
            let err = parse(&["--format", f]).err().unwrap();
            assert!(err.contains(&format!("--format {f}")), "{err}");
            assert!(err.contains("writes .dendro only"), "{err}");
            assert!(err.contains("5.x recorder"), "{err}");
        }
        let err = parse(&["--format", "csv"]).err().unwrap();
        assert!(err.contains("unknown format"), "{err}");
    }

    /// `--separate` is refused with any number of endpoints, rather than
    /// ignored.
    #[test]
    fn separate_is_refused() {
        for args in [
            &["--separate"][..],
            &[
                "--separate",
                "--endpoint",
                "http://a:1",
                "--endpoint",
                "http://b:1",
            ],
        ] {
            let err = parse(args).err().unwrap();
            assert!(err.contains("--separate"), "{err}");
            assert!(err.contains("5.x recorder"), "{err}");
        }
    }

    /// A config file is held to the same rules: its output must be a
    /// `.dendro`, its `format` must be `dendro`, and `separate = true` is
    /// refused. `-o` on the command line overrides its output.
    #[test]
    fn a_config_file_is_held_to_the_same_rules() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, recording: &str| {
            let path = dir.path().join(name);
            std::fs::write(
                &path,
                format!(
                    "[recording]\n{recording}\n\n[[endpoints]]\nurl = \"http://localhost:4241\"\n"
                ),
            )
            .unwrap();
            path.to_str().unwrap().to_string()
        };

        let ok = write("ok.toml", "output = \"run.dendro\"\nformat = \"dendro\"");
        let cfg = parse(&["--config", &ok]).unwrap();
        assert_eq!(cfg.output, PathBuf::from("run.dendro"));

        let parquet = write("parquet.toml", "output = \"run.parquet\"");
        let err = parse(&["--config", &parquet]).err().unwrap();
        assert!(
            err.starts_with(&format!("in {parquet}: output = \"run.parquet\": ")),
            "{err}"
        );
        assert!(err.contains("5.x recorder"), "{err}");
        let dat = write("dat.toml", "output = \"run.dat\"");
        let err = parse(&["--config", &dat]).err().unwrap();
        assert!(
            err.starts_with(&format!("in {dat}: output = \"run.dat\": ")),
            "{err}"
        );
        assert_eq!(
            parse(&["--config", &parquet, "-o", "run.dendro"])
                .unwrap()
                .output,
            PathBuf::from("run.dendro")
        );

        let rez = write("rez.toml", "output = \"run.dendro\"\nformat = \"rez\"");
        let err = parse(&["--config", &rez]).err().unwrap();
        assert!(err.contains("--format rez"), "{err}");

        let separate = write("separate.toml", "output = \"run.dendro\"\nseparate = true");
        let err = parse(&["--config", &separate]).err().unwrap();
        assert!(err.contains("--separate"), "{err}");
        let not_separate = write("not.toml", "output = \"run.dendro\"\nseparate = false");
        assert!(parse(&["--config", &not_separate]).is_ok());
    }

    /// `--stream` was removed before 6.0.0: the output format decides the
    /// transport. A script still passing it fails as clap fails on any
    /// unknown argument, rather than having it silently ignored.
    #[test]
    fn stream_is_rejected_as_an_unknown_argument() {
        let err = crate::recorder::command()
            .try_get_matches_from(["record", "--stream"])
            .expect_err("--stream is not an argument");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert_eq!(err.exit_code(), 2);
        let help = crate::recorder::command().render_long_help().to_string();
        assert!(!help.contains("--stream"), "{help}");
    }

    /// `--rez-version` is gone, and a script still passing it must FAIL rather
    /// than be silently ignored.
    #[test]
    fn rez_version_is_rejected() {
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
    /// ever written for either. The keys they were meant to set are plain file
    /// metadata (`node`, `instance`, the same ones `recording combine` reads),
    /// so `--metadata node=web-01` was always the working form.
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
    fn resolve_output_none_when_neither_is_given() {
        let (out, dep) = resolve_output(None, None).unwrap();
        assert_eq!(out, None);
        assert!(!dep);
    }

    #[test]
    fn resolve_output_positional_is_deprecated() {
        let pos = PathBuf::from("legacy.dendro");
        let (out, dep) = resolve_output(None, Some(&pos)).unwrap();
        assert_eq!(out, Some(pos));
        assert!(dep);
    }

    #[test]
    fn resolve_output_both_is_error() {
        let flag = PathBuf::from("new.dendro");
        let pos = PathBuf::from("legacy.dendro");
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
