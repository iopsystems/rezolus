//! Known defects in metrics that agents of a range of versions recorded.
//!
//! An erratum names the metrics it covers and the first agent version without
//! the defect. A recording carries the version of the agent that was recorded
//! (`version` metadata), so a reader checks this table when it opens a
//! recording; the table is compiled in, so it covers recordings made before
//! the defect was known. An erratum annotates a metric and never changes a
//! value.
//!
//! Add an entry for a defect: a value that a fixed agent records differently,
//! or a metric that a fixed agent records alongside a counter needed to
//! interpret it ([`Severity::Misleading`]). Behaviour that every version has
//! and nothing in a fixed agent corrects belongs in the metric's description.

use std::fmt;

/// How far a reader can rely on the values an erratum covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The values are wrong.
    Wrong,
    /// The values are correct by their definition, and the recording lacks
    /// what a reader needs to interpret them.
    Misleading,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Wrong => "wrong",
            Severity::Misleading => "misleading",
        })
    }
}

/// One defect, the metrics it affects, and the agent version that fixed it.
#[derive(Debug, PartialEq, Eq)]
pub struct Erratum {
    /// The metrics whose values the defect affects.
    pub metrics: &'static [&'static str],
    /// The first agent version that records these metrics without the defect.
    /// A fix backported to an older release line needs that line's fixed
    /// version expressed here too; one version covers main's line only.
    pub fixed_in: &'static str,
    /// The first agent version with the defect, when agents before it did not
    /// have it. A recording without a usable version is reported either way.
    pub since: Option<&'static str>,
    /// Metrics that only agents at or after `fixed_in` record. A recording
    /// without a usable version that holds one of them is from a fixed agent.
    pub fixed_if_present: &'static [&'static str],
    pub severity: Severity,
    /// What is wrong with the values, in one or two sentences.
    pub summary: &'static str,
    /// The pull request that fixed it.
    pub reference: &'static str,
}

/// Every known erratum.
pub static ERRATA: &[Erratum] = &[
    Erratum {
        metrics: &[
            "network_ena_bandwidth_allowance_exceeded",
            "network_ena_pps_allowance_exceeded",
            "network_ena_conntrack_allowance_exceeded",
            "network_ena_linklocal_allowance_exceeded",
        ],
        fixed_in: "5.20.1-alpha.4",
        since: None,
        fixed_if_present: &[],
        severity: Severity::Wrong,
        summary: "On a host with more than one ENA interface, each value is the counter of \
                  one interface, chosen by directory order and possibly different after an \
                  agent restart. The other interfaces' counts were not recorded.",
        reference: "https://github.com/iopsystems/rezolus/pull/1216",
    },
    Erratum {
        metrics: &["network_bytes", "network_packets"],
        fixed_in: "5.20.1-alpha.3",
        since: None,
        fixed_if_present: &["network_host_bytes", "network_host_packets"],
        severity: Severity::Misleading,
        summary: "Counted once per network interface a packet passes through, including \
                  loopback, bridges, bonds and VLANs: about 2x the wire traffic on transmit \
                  for a bonded host, more on a host running VMs or containers. The recording \
                  has no network_host_bytes or network_host_packets, which count traffic \
                  crossing the host boundary once.",
        reference: "https://github.com/iopsystems/rezolus/pull/1214",
    },
    Erratum {
        metrics: &[
            "network_ena_bandwidth_allowance_exceeded",
            "network_ena_pps_allowance_exceeded",
            "network_ena_conntrack_allowance_exceeded",
            "network_ena_linklocal_allowance_exceeded",
        ],
        fixed_in: "6.0.0-alpha.169",
        since: Some("5.20.1-alpha.4"),
        fixed_if_present: &[],
        severity: Severity::Misleading,
        summary: "A counter that was zero was not recorded, so an interface or direction \
                  missing from these series may have had its allowance never exceeded, \
                  rather than no ENA statistics.",
        reference: "https://github.com/iopsystems/rezolus/pull/1736",
    },
];

/// Whether an erratum applies to a recording, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applies {
    /// The recording's agent version is older than the fix.
    Affected,
    /// The recording has no agent version, so it may be from an affected
    /// agent.
    NoVersion,
    /// The recording's version (the string held) is not a semver version, so
    /// it may be from an affected agent.
    Unparseable(String),
}

/// An erratum that applies to a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub erratum: &'static Erratum,
    pub applies: Applies,
    /// The erratum's metrics that the recording holds.
    pub present: Vec<&'static str>,
}

impl Finding {
    /// Why the erratum applies: the agent version range, and whether the
    /// recording's version placed it there or is missing or unparseable.
    pub fn condition(&self) -> String {
        let fixed = self.erratum.fixed_in;
        let range = match self.erratum.since {
            Some(since) => format!("from {since}, before {fixed}"),
            None => format!("before {fixed}"),
        };
        match &self.applies {
            Applies::Affected => format!("recorded by an agent {range}"),
            Applies::NoVersion => {
                format!("the recording has no Rezolus agent version; agents {range} recorded this")
            }
            Applies::Unparseable(v) => format!(
                "the recording's agent version ({v}) is not a semver version; agents {range} \
                 recorded this"
            ),
        }
    }

    /// One line: severity, the condition, the summary and the reference.
    pub fn describe(&self) -> String {
        let condition = self.condition();
        format!(
            "{} ({condition}): {} {}",
            self.erratum.severity, self.erratum.summary, self.erratum.reference
        )
    }
}

/// The errata that apply to a recording made by an agent of `version` (its
/// `version` metadata, empty when absent) and holding the metrics for which
/// `has_metric` returns true. An erratum applies when the recording holds at
/// least one of its metrics and the version is older than the fix, or when
/// there is no usable version and the recording holds none of the erratum's
/// `fixed_if_present` metrics.
pub fn applicable(version: &str, has_metric: impl Fn(&str) -> bool) -> Vec<Finding> {
    let trimmed = version.trim();
    let parsed = semver::Version::parse(trimmed.strip_prefix('v').unwrap_or(trimmed));
    ERRATA
        .iter()
        .filter_map(|erratum| {
            let present: Vec<&'static str> = erratum
                .metrics
                .iter()
                .copied()
                .filter(|m| has_metric(m))
                .collect();
            if present.is_empty() {
                return None;
            }
            let applies = match &parsed {
                Ok(v) => {
                    let fixed = semver::Version::parse(erratum.fixed_in)
                        .expect("an erratum's fixed_in is a semver version");
                    if *v >= fixed {
                        return None;
                    }
                    if let Some(since) = erratum.since {
                        let since = semver::Version::parse(since)
                            .expect("an erratum's since is a semver version");
                        if *v < since {
                            return None;
                        }
                    }
                    Applies::Affected
                }
                Err(_) if erratum.fixed_if_present.iter().any(|m| has_metric(m)) => {
                    return None;
                }
                Err(_) if trimmed.is_empty() => Applies::NoVersion,
                Err(_) => Applies::Unparseable(trimmed.to_string()),
            };
            Some(Finding {
                erratum,
                applies,
                present,
            })
        })
        .collect()
}

/// The errata that apply to `data`: [`applicable`] with its `version`
/// metadata when it is a Rezolus agent's recording
/// ([`crate::source_kind::is_rezolus_recording`]) and no version otherwise,
/// since another producer's `version` is not an agent version.
pub fn for_source(data: &dyn metriken_query::MetricsSource) -> Vec<Finding> {
    let names: std::collections::HashSet<String> = data
        .counter_names()
        .into_iter()
        .chain(data.gauge_names())
        .chain(data.histogram_names())
        .chain(data.gauge_histogram_names())
        .collect();
    if !ERRATA
        .iter()
        .any(|e| e.metrics.iter().any(|m| names.contains(*m)))
    {
        return Vec::new();
    }
    let version = if crate::source_kind::is_rezolus_recording(data) {
        data.version()
    } else {
        String::new()
    };
    applicable(&version, |m| names.contains(m))
}

/// The findings in `findings` that cover `metric` and whose recording holds
/// it.
pub fn for_metric<'a>(
    findings: &'a [Finding],
    metric: &'a str,
) -> impl Iterator<Item = &'a Finding> + 'a {
    findings.iter().filter(move |f| f.present.contains(&metric))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |m| names.contains(&m)
    }

    #[test]
    fn every_entry_is_well_formed() {
        for e in ERRATA {
            let fixed = semver::Version::parse(e.fixed_in).unwrap();
            if let Some(since) = e.since {
                assert!(semver::Version::parse(since).unwrap() < fixed);
            }
            assert!(!e.metrics.is_empty());
            for m in e.fixed_if_present {
                assert!(!e.metrics.contains(m), "{m} both affected and a fix marker");
            }
        }
    }

    #[test]
    fn older_agent_is_affected() {
        let f = applicable("5.20.0", has(&["network_bytes"]));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].applies, Applies::Affected);
        assert_eq!(f[0].erratum.reference, ERRATA[1].reference);
        assert_eq!(f[0].present, vec!["network_bytes"]);
    }

    #[test]
    fn prerelease_before_the_fix_is_affected_and_the_fix_is_not() {
        let m = has(&["network_bytes"]);
        assert_eq!(applicable("5.20.1-alpha.2", &m).len(), 1);
        assert!(applicable("5.20.1-alpha.3", &m).is_empty());
        assert!(applicable("5.20.1", &m).is_empty());
        assert!(applicable("6.0.0-alpha.1", &m).is_empty());
    }

    #[test]
    fn since_bounds_the_affected_range() {
        let m = has(&["network_ena_pps_allowance_exceeded"]);
        let refs = |v: &str| -> Vec<&str> {
            applicable(v, &m)
                .iter()
                .map(|f| f.erratum.reference)
                .collect()
        };
        // Before alpha.4: only the single-interface defect.
        assert_eq!(refs("5.20.0"), vec![ERRATA[0].reference]);
        // From alpha.4 to the zero fix: only the zero defect.
        assert_eq!(refs("5.20.1-alpha.4"), vec![ERRATA[2].reference]);
        assert_eq!(refs("6.0.0-alpha.168"), vec![ERRATA[2].reference]);
        assert!(refs("6.0.0-alpha.169").is_empty());
        // No version: both may apply.
        assert_eq!(refs("").len(), 2);
        let f = applicable("6.0.0-alpha.1", &m);
        assert!(f[0].describe().starts_with(
            "misleading (recorded by an agent from 5.20.1-alpha.4, before 6.0.0-alpha.169)"
        ));
        let f = applicable("", &m);
        assert!(f[1].describe().contains(
            "has no Rezolus agent version; agents from 5.20.1-alpha.4, before 6.0.0-alpha.169 recorded this"
        ));
    }

    #[test]
    fn a_leading_v_is_accepted() {
        assert!(applicable("v5.21.0", has(&["network_bytes"])).is_empty());
        assert_eq!(applicable("v5.19.1", has(&["network_bytes"])).len(), 1);
    }

    #[test]
    fn missing_version_is_no_version() {
        for v in ["", "  "] {
            let f = applicable(v, has(&["network_bytes"]));
            assert_eq!(f.len(), 1, "{v:?}");
            assert_eq!(f[0].applies, Applies::NoVersion);
            assert!(f[0].describe().contains("has no Rezolus agent version"));
        }
    }

    #[test]
    fn unparseable_version_is_named() {
        let f = applicable("5.19", has(&["network_bytes"]));
        assert_eq!(f[0].applies, Applies::Unparseable("5.19".to_string()));
        assert!(
            f[0].describe()
                .contains("the recording's agent version (5.19) is not a semver version")
        );
    }

    #[test]
    fn a_fix_marker_clears_an_unknown_version_but_not_a_known_one() {
        let m = has(&["network_bytes", "network_host_bytes"]);
        assert!(applicable("", &m).is_empty());
        assert!(applicable("garbage", &m).is_empty());
        // A parsed version decides on its own.
        assert_eq!(applicable("5.19.0", &m).len(), 1);
    }

    #[test]
    fn no_finding_without_an_affected_metric() {
        assert!(applicable("5.0.0", has(&["cpu_usage"])).is_empty());
        assert!(applicable("", has(&[])).is_empty());
    }

    #[test]
    fn for_metric_selects_metrics_the_recording_holds() {
        let f = applicable(
            "5.19.0",
            has(&["network_bytes", "network_ena_pps_allowance_exceeded"]),
        );
        assert_eq!(f.len(), 2);
        assert_eq!(for_metric(&f, "network_bytes").count(), 1);
        // Covered by the erratum, absent from the recording.
        assert_eq!(for_metric(&f, "network_packets").count(), 0);
        assert_eq!(for_metric(&f, "cpu_usage").count(), 0);
        assert_eq!(f[0].present, vec!["network_ena_pps_allowance_exceeded"]);
    }

    #[test]
    fn describe_names_the_condition() {
        let f = applicable("5.19.0", has(&["network_bytes"]));
        assert!(
            f[0].describe()
                .starts_with("misleading (recorded by an agent before 5.20.1-alpha.3)")
        );
    }
}
