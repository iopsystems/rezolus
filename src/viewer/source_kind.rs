//! Per-source classification for the viewer. Detection lives here (never in the
//! dashboard crate). Layered: metadata markers first, then a cross-platform
//! self-sampler fingerprint, else Simple.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Rezolus,
    Service,
    Simple,
}

/// Cross-platform Rezolus self-telemetry (rezolus/rusage sampler). Present on
/// both Linux and macOS. Deliberately excludes rezolus_bpf_* (Linux/eBPF-only).
pub const REZOLUS_SELF_ANCHORS: &[&str] = &[
    "rezolus_cpu_usage",
    "rezolus_memory_usage_resident_set_size",
    "rezolus_rusage",
];

pub fn detect_source_kind(
    source: &str,
    has_sampler_status: bool,
    has_template: bool,
    metric_names: &[String],
) -> SourceKind {
    // Tier 1: metadata markers (source tag, sampler_status). Drop spoofable systeminfo.
    if source == "rezolus" || has_sampler_status {
        return SourceKind::Rezolus;
    }
    if has_template {
        return SourceKind::Service;
    }
    // Tier 2: cross-platform self-sampler fingerprint.
    if metric_names.iter().any(|n| REZOLUS_SELF_ANCHORS.contains(&n.as_str())) {
        return SourceKind::Rezolus;
    }
    SourceKind::Simple
}

/// Resolve the display/section name for a source. Rezolus: node else "rezolus".
/// Simple: explicit source -> filename stem -> "metrics".
pub fn resolve_source_name(
    kind: SourceKind,
    source: &str,
    node: Option<&str>,
    filename_stem: Option<&str>,
) -> String {
    match kind {
        SourceKind::Rezolus => node.filter(|s| !s.is_empty()).unwrap_or("rezolus").to_string(),
        SourceKind::Service => source.to_string(),
        SourceKind::Simple => {
            if !source.is_empty() {
                source.to_string()
            } else if let Some(stem) = filename_stem.filter(|s| !s.is_empty()) {
                stem.to_string()
            } else {
                "metrics".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn metadata_marker_source_rezolus() {
        assert_eq!(detect_source_kind("rezolus", false, false, &[]), SourceKind::Rezolus);
    }

    #[test]
    fn metadata_marker_sampler_status() {
        assert_eq!(detect_source_kind("anything", true, false, &[]), SourceKind::Rezolus);
    }

    #[test]
    fn template_makes_service() {
        assert_eq!(detect_source_kind("llm-perf", false, true, &[]), SourceKind::Service);
    }

    #[test]
    fn self_sampler_fingerprint_linux() {
        let m = names(&["rezolus_cpu_usage", "rezolus_rusage", "cpu_usage"]);
        assert_eq!(detect_source_kind("", false, false, &m), SourceKind::Rezolus);
    }

    #[test]
    fn self_sampler_fingerprint_macos_no_bpf() {
        // macOS recording: rusage self-metrics present, NO rezolus_bpf_* at all.
        let m = names(&["rezolus_cpu_usage", "rezolus_memory_usage_resident_set_size"]);
        assert_eq!(detect_source_kind("", false, false, &m), SourceKind::Rezolus);
    }

    #[test]
    fn foreign_metrics_are_simple() {
        let m = names(&["http_requests_total", "queue_depth"]);
        assert_eq!(detect_source_kind("", false, false, &m), SourceKind::Simple);
    }

    #[test]
    fn name_resolution() {
        assert_eq!(resolve_source_name(SourceKind::Rezolus, "rezolus", Some("node7"), None), "node7");
        assert_eq!(resolve_source_name(SourceKind::Rezolus, "rezolus", None, Some("x")), "rezolus");
        assert_eq!(resolve_source_name(SourceKind::Simple, "svc", None, Some("cap")), "svc");
        assert_eq!(resolve_source_name(SourceKind::Simple, "", None, Some("cap")), "cap");
        assert_eq!(resolve_source_name(SourceKind::Simple, "", None, None), "metrics");
    }
}
