//! `rezolus samplers <endpoint>` — fetch /samplers and print a classified,
//! human-readable health rollup.

use crate::agent::sampler_status::{ProbeVerdict, SamplerHealth, SamplerState, SamplerStatus};

/// Render the rollup as plain text. Returns (text, has_problem) where
/// has_problem is true if any sampler is degraded or failed (drives the
/// process exit code). `unsupported` is informational and does NOT set it.
pub fn render(statuses: &[SamplerStatus]) -> (String, bool) {
    let mut out = String::new();
    let mut problem = false;
    for s in statuses {
        let (label, detail) = match (&s.state, s.health) {
            (SamplerState::Disabled, _) => ("disabled".to_string(), String::new()),
            (SamplerState::Failed { error }, _) => {
                problem = true;
                ("failed".to_string(), error.clone())
            }
            (_, Some(SamplerHealth::Failed)) => {
                problem = true;
                ("failed".to_string(), String::new())
            }
            (_, Some(SamplerHealth::Degraded)) => {
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
                ("degraded".to_string(), d)
            }
            (_, Some(SamplerHealth::Unsupported)) => {
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
                ("unsupported".to_string(), d)
            }
            (_, Some(SamplerHealth::Healthy)) | (SamplerState::Active, None) => {
                ("healthy".to_string(), String::new())
            }
        };
        if detail.is_empty() {
            out.push_str(&format!("{:<22} {}\n", s.name, label));
        } else {
            out.push_str(&format!("{:<22} {:<12} {}\n", s.name, label, detail));
        }
    }
    (out, problem)
}

use clap::{value_parser, Arg, ArgAction, Command};

pub fn command() -> Command {
    Command::new("samplers")
        .about("Fetch and classify sampler health from a running agent")
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
                .help("Emit the raw /samplers JSON")
                .action(ArgAction::SetTrue),
        )
}

pub fn run(args: &clap::ArgMatches) {
    let endpoint = args.get_one::<String>("ENDPOINT").unwrap();
    let json = args.get_flag("json");
    let url = format!("{}/samplers", endpoint.trim_end_matches('/'));

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

    let statuses: Vec<SamplerStatus> = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("parsing /samplers response: {e}");
            std::process::exit(1);
        }
    };

    let (text, problem) = render(&statuses);
    print!("{text}");
    if problem {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sampler_status::{ProbeIntent, ProgramStatus};

    #[test]
    fn degraded_sets_problem_flag_and_lists_probe() {
        let statuses = vec![SamplerStatus {
            name: "network_interfaces".into(),
            state: SamplerState::Active,
            health: Some(SamplerHealth::Degraded),
            programs: vec![ProgramStatus {
                name: "ena_tx_timeout".into(),
                attached: false,
                error: Some("not attached".into()),
                intent: Some(ProbeIntent::Driver {
                    module: "ena".into(),
                }),
                label: None,
                expected: true,
                verdict: ProbeVerdict::Broken,
            }],
        }];
        let (text, problem) = render(&statuses);
        assert!(problem);
        assert!(text.contains("degraded"));
        assert!(text.contains("ena_tx_timeout"));
    }

    #[test]
    fn unsupported_does_not_set_problem_flag() {
        let statuses = vec![SamplerStatus {
            name: "cpu_usage".into(),
            state: SamplerState::Active,
            health: Some(SamplerHealth::Unsupported),
            programs: vec![ProgramStatus {
                name: "cpuacct_account_field_kprobe".into(),
                attached: false,
                error: Some("no kernel support (ENOENT)".into()),
                intent: Some(ProbeIntent::Required),
                label: Some("CPU time by category".into()),
                expected: false,
                verdict: ProbeVerdict::Unsupported,
            }],
        }];
        let (text, problem) = render(&statuses);
        assert!(!problem);
        assert!(text.contains("unsupported"));
        assert!(text.contains("CPU time by category"));
    }

    #[test]
    fn healthy_and_disabled_render_without_problem() {
        let statuses = vec![
            SamplerStatus {
                name: "tcp_traffic".into(),
                state: SamplerState::Active,
                health: Some(SamplerHealth::Healthy),
                programs: vec![],
            },
            SamplerStatus {
                name: "gpu".into(),
                state: SamplerState::Disabled,
                health: None,
                programs: vec![],
            },
        ];
        let (text, problem) = render(&statuses);
        assert!(!problem);
        assert!(text.contains("healthy"));
        assert!(text.contains("disabled"));
    }
}
