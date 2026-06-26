//! `rezolus status <endpoint>` — fetch /status and print an agent overview:
//! version, uptime, snapshot TTL, and a sampler-health rollup.

use crate::agent::sampler_status::{
    AgentStatus, ProbeVerdict, SamplerHealth, SamplerState, SamplerStatus,
};
use std::time::Duration;

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

use clap::{value_parser, Arg, ArgAction, Command};

pub fn command() -> Command {
    Command::new("status")
        .about("Fetch and display agent status (version, uptime, sampler health)")
        .arg(
            Arg::new("ENDPOINT")
                .help("Agent base URL, e.g. http://localhost:4241")
                .required(true)
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
    let endpoint = args.get_one::<String>("ENDPOINT").unwrap();
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
