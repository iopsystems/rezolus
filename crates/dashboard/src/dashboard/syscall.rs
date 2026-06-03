use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut syscall = Group::new("Syscall", "syscall");
    syscall
        .metadata
        .insert("no_collapse".to_string(), serde_json::json!(true));

    let overall = syscall.subgroup("Overall");
    overall.describe("Aggregate syscall rate and latency across all operation categories.");
    overall.histogram_rate_mean(
        "Overall",
        "syscall-total",
        "syscall_latency",
        RateSource::Counter("sum(irate(syscall[5m]))".to_string()),
        Unit::Time,
    );
    overall.plot_promql(
        PlotOpts::histogram_latency("Overall Latency", "syscall-total-latency"),
        "syscall_latency".to_string(),
    );

    for op in &[
        "Read",
        "Write",
        "Poll",
        "Socket",
        "Lock",
        "Time",
        "Sleep",
        "Yield",
        "Filesystem",
        "Memory",
        "Process",
        "Query",
        "IPC",
        "Timer",
        "Event",
        "Other",
    ] {
        let op_lower = op.to_lowercase();
        let sg = syscall.subgroup(*op);
        sg.histogram_rate_mean(
            op,
            &format!("syscall-{op_lower}"),
            &format!("syscall_latency{{op=\"{op_lower}\"}}"),
            RateSource::Counter(format!("sum(irate(syscall{{op=\"{op_lower}\"}}[5m]))")),
            Unit::Time,
        );
        sg.plot_promql(
            PlotOpts::histogram_latency(format!("{op} Latency"), format!("syscall-{op}-latency")),
            format!("syscall_latency{{op=\"{op_lower}\"}}"),
        );
    }

    view.group(syscall);

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;

    #[test]
    fn syscall_overall_and_per_op_get_rate_mean_pairs() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        let json = serde_json::to_string(&view).unwrap().replace("\\\"", "\"");
        // Overall: rate query preserved verbatim, plus mean.
        assert!(json.contains("sum(irate(syscall[5m]))"));
        assert!(json.contains("histogram_mean(syscall_latency)\""));
        // Per-op (read): rate preserved, mean added.
        assert!(json.contains("sum(irate(syscall{op=\"read\"}[5m]))"));
        assert!(json.contains("histogram_mean(syscall_latency{op=\"read\"})"));
        // Percentile histogram still present.
        assert!(json.contains("syscall_latency{op=\"write\"}"));
        // No duplicate standalone overall rate: the overall rate query
        // string appears exactly once.
        assert_eq!(json.matches("sum(irate(syscall[5m]))").count(), 1);
    }
}
