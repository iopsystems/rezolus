//! `rezolus status <endpoint>` — fetch /status and print an agent overview:
//! version, uptime, snapshot TTL, and a sampler-health rollup.

use crate::agent::sampler_status::{
    AgentStatus, ProbeVerdict, SamplerHealth, SamplerState, SamplerStatus,
};
use clap::{value_parser, Arg, ArgAction, Command};
use std::time::Duration;

/// The agent on this machine — the same endpoint `rezolus record --url`
/// defaults to, so the two agree about what "no endpoint given" means.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:4241";

/// The agent's default port, supplied when an endpoint names only a host.
const DEFAULT_PORT: u16 = 4241;

/// Accept the short forms people actually type.
///
/// `reqwest` needs an absolute URL, so a bare `web-01` or `web-01:4241` used to
/// fail with "builder error for url" — which names neither the problem nor the
/// fix. Anything without a scheme gets `http://`, and a host with no port gets
/// the agent's. An endpoint that already carries a scheme is left exactly as
/// given: a caller who wrote `https://` or a path prefix means it.
pub fn normalize_endpoint(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        return trimmed.to_string();
    }
    // A port is a colon followed by digits only — this must not mistake the
    // colon in a bare IPv6 literal for one.
    let has_port = trimmed
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    if has_port {
        format!("http://{trimmed}")
    } else {
        format!("http://{trimmed}:{DEFAULT_PORT}")
    }
}

/// Render the one-line agent header: version, humanized uptime, humanized ttl.
pub fn render_header(status: &AgentStatus) -> String {
    format!(
        "Rezolus {}   up {}   ttl {}",
        status.version,
        humantime::format_duration(Duration::from_secs(status.uptime_seconds)),
        humantime::format_duration(Duration::from_secs(status.ttl_seconds)),
    )
}

/// Render the tally line plus one line per NON-healthy sampler. Returns
/// (text, has_problem) where has_problem is true iff any sampler is degraded
/// or failed (drives the process exit code). `unsupported` is informational.
pub fn render_samplers(samplers: &[SamplerStatus]) -> (String, bool) {
    let mut healthy = 0usize;
    let mut unsupported = 0usize;
    let mut degraded = 0usize;
    let mut failed = 0usize;
    let mut lines = String::new();
    let mut problem = false;

    for s in samplers {
        match (&s.state, s.health) {
            (SamplerState::Disabled, _) => {} // not counted in the health tally
            // Not a fault: too few PMU counters for its whole event set. Shown
            // with the numbers so the trade is visible and re-rankable.
            (SamplerState::PmuLimited { cpus, of }, _) => {
                degraded += 1;
                problem = true;
                lines.push_str(&format!(
                    "  {:<22} {:<12} {cpus} of {of} cpus\n",
                    s.name, "pmu-limited"
                ));
            }
            (SamplerState::PmuStarved { wants, free }, _) => {
                unsupported += 1;
                lines.push_str(&format!(
                    "  {:<22} {:<12} needs {wants}/cpu, {free} free\n",
                    s.name, "pmu-starved"
                ));
            }
            (SamplerState::Failed { error }, _) => {
                failed += 1;
                problem = true;
                lines.push_str(&format!("  {:<22} {:<12} {}\n", s.name, "failed", error));
            }
            (_, Some(SamplerHealth::Failed)) => {
                failed += 1;
                problem = true;
                lines.push_str(&format!("  {:<22} {}\n", s.name, "failed"));
            }
            (_, Some(SamplerHealth::Degraded)) => {
                degraded += 1;
                problem = true;
                let d = s
                    .programs
                    .iter()
                    .filter(|p| p.verdict == ProbeVerdict::Broken)
                    .map(|p| {
                        format!(
                            "{} {}",
                            p.label.as_deref().unwrap_or(&p.name),
                            p.error.as_deref().unwrap_or("not attached")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push_str(&format!("  {:<22} {:<12} {}\n", s.name, "degraded", d));
            }
            (_, Some(SamplerHealth::Unsupported)) => {
                unsupported += 1;
                let d = s
                    .programs
                    .iter()
                    .filter(|p| p.verdict == ProbeVerdict::Unsupported)
                    .map(|p| {
                        format!(
                            "{} unavailable (no kernel support)",
                            p.label.as_deref().unwrap_or(&p.name)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push_str(&format!("  {:<22} {:<12} {}\n", s.name, "unsupported", d));
            }
            (_, Some(SamplerHealth::Healthy)) | (SamplerState::Active, None) => healthy += 1,
        }
    }

    let tally = format!(
        "{} healthy, {} unsupported, {} degraded, {} failed\n",
        healthy, unsupported, degraded, failed
    );
    (format!("{tally}{lines}"), problem)
}

pub fn command() -> Command {
    Command::new("status")
        .about("Fetch and display agent status (version, uptime, sampler health)")
        .long_about(
            "Ask a running agent how it is doing. With no argument it asks the agent on\n\
             this machine (http://localhost:4241), the same endpoint `rezolus record`\n\
             defaults to.\n\n\
             Prints one header line — version, uptime\n\
             and the snapshot TTL — then a tally of samplers by state, and one line for each\n\
             sampler that is NOT healthy, with the reason.\n\n\
             States: healthy, degraded (running, but a probe or some CPUs are missing),\n\
             failed (not running), unsupported (this kernel or machine cannot run it),\n\
             pmu-limited (running on fewer CPUs than asked, because PMU counters are\n\
             scarce) and pmu-starved (no counters left for it at all). Disabled samplers\n\
             are not counted.\n\n\
             EXIT STATUS: 0 when nothing is wrong, 1 when any sampler is degraded, failed\n\
             or pmu-limited — so this works as a health check in a script or a readiness\n\
             probe. A pmu-starved or disabled sampler on its own does not fail the check.\n\
             A network or parse error also exits non-zero, with the reason on stderr.\n\n\
             EXAMPLES:\n    \
             # Is the agent on this machine healthy?\n    \
             rezolus status\n\n    \
             # Another host, written the short way\n    \
             rezolus status web-01\n\n    \
             # Use it as a gate in a script\n    \
             rezolus status || echo \"agent is unhealthy\"\n\n    \
             # The raw payload, for a machine to read\n    \
             rezolus status --json",
        )
        .arg(
            Arg::new("ENDPOINT")
                .help("Agent to ask (default http://localhost:4241). A bare host or host:port is fine — the scheme and the default port are filled in, so `rezolus status web-01` works")
                .required(false)
                .index(1)
                .value_parser(value_parser!(String)),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .help("Emit the raw /status JSON")
                .action(ArgAction::SetTrue),
        )
}

pub fn run(args: &clap::ArgMatches) {
    let endpoint = args
        .get_one::<String>("ENDPOINT")
        .map(|e| normalize_endpoint(e))
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
    let json = args.get_flag("json");
    let url = format!("{}/status", endpoint.trim_end_matches('/'));

    let body = match reqwest::blocking::get(&url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("fetching {url}: {e}");
            std::process::exit(1);
        }
    };

    if json {
        println!("{body}");
        return;
    }

    let status: AgentStatus = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("parsing /status response: {e}");
            std::process::exit(1);
        }
    };

    println!("{}", render_header(&status));
    let (text, problem) = render_samplers(&status.samplers);
    print!("{text}");
    if problem {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sampler_status::{ProbeIntent, ProgramStatus};

    fn prog(verdict: ProbeVerdict, label: Option<&str>, name: &str) -> ProgramStatus {
        ProgramStatus {
            name: name.into(),
            attached: false,
            error: Some("not attached".into()),
            intent: Some(ProbeIntent::Required),
            label: label.map(|s| s.into()),
            expected: true,
            verdict,
        }
    }

    /// The short forms people type, and the one case that must NOT be
    /// treated as a port: a bare IPv6 literal is all colons.
    #[test]
    fn normalize_endpoint_fills_in_scheme_and_port() {
        // host only
        assert_eq!(normalize_endpoint("localhost"), "http://localhost:4241");
        assert_eq!(normalize_endpoint("web-01"), "http://web-01:4241");
        assert_eq!(normalize_endpoint("127.0.0.1"), "http://127.0.0.1:4241");
        // host:port
        assert_eq!(
            normalize_endpoint("localhost:9999"),
            "http://localhost:9999"
        );
        // already absolute — left alone, scheme and path included
        for already in [
            "http://localhost:4241",
            "https://agent.internal:4241",
            "http://host:4241/prefix",
        ] {
            assert_eq!(normalize_endpoint(already), already);
        }
        // trailing slash and surrounding space are noise
        assert_eq!(
            normalize_endpoint("  localhost:4241/  "),
            "http://localhost:4241"
        );
        // an IPv6 literal's colons are not a port
        assert_eq!(normalize_endpoint("[::1]"), "http://[::1]:4241");
        assert_eq!(normalize_endpoint("[::1]:4241"), "http://[::1]:4241");
    }

    /// The no-argument default and `record`'s must not drift apart: "the agent
    /// on this machine" has to mean one thing across the binary.
    #[test]
    fn default_endpoint_matches_the_recorders() {
        assert_eq!(DEFAULT_ENDPOINT, "http://localhost:4241");
    }

    #[test]
    fn header_formats_version_uptime_ttl() {
        let s = AgentStatus {
            version: "5.15.1".into(),
            uptime_seconds: 11532,
            ttl_seconds: 60,
            samplers: vec![],
        };
        let h = render_header(&s);
        assert!(h.contains("Rezolus 5.15.1"));
        assert!(h.contains("up 3h 12m 12s"));
        assert!(h.contains("ttl 1m"));
    }

    #[test]
    fn degraded_sets_problem_and_lists_only_nonhealthy() {
        let samplers = vec![
            SamplerStatus {
                name: "tcp_traffic".into(),
                state: SamplerState::Active,
                health: Some(SamplerHealth::Healthy),
                programs: vec![],
            },
            SamplerStatus {
                name: "network_interfaces".into(),
                state: SamplerState::Active,
                health: Some(SamplerHealth::Degraded),
                programs: vec![prog(ProbeVerdict::Broken, None, "ena_tx_timeout")],
            },
        ];
        let (text, problem) = render_samplers(&samplers);
        assert!(problem);
        assert!(text.contains("1 healthy, 0 unsupported, 1 degraded, 0 failed"));
        assert!(text.contains("network_interfaces"));
        assert!(text.contains("ena_tx_timeout"));
        assert!(!text.contains("tcp_traffic"));
    }

    #[test]
    fn unsupported_does_not_set_problem() {
        let samplers = vec![SamplerStatus {
            name: "cpu_usage".into(),
            state: SamplerState::Active,
            health: Some(SamplerHealth::Unsupported),
            programs: vec![prog(
                ProbeVerdict::Unsupported,
                Some("CPU time by category"),
                "cpuacct_account_field_kprobe",
            )],
        }];
        let (text, problem) = render_samplers(&samplers);
        assert!(!problem);
        assert!(text.contains("0 healthy, 1 unsupported, 0 degraded, 0 failed"));
        assert!(text.contains("CPU time by category"));
    }
}
