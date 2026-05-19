//! Snapshot tests for every public SQL emitter in `dashboard::sql`.
//!
//! These pin the exact SQL string each emitter produces for a
//! representative argument set. They are the only direct unit signal
//! on `crates/dashboard/src/sql.rs` (~715 LOC, 16 functions, used by
//! ~180 plot calls in `crates/dashboard/src/dashboard/*.rs`); without
//! them the only feedback on emitter drift is end-to-end integration
//! tests against real parquets (chromium per-section smoke + the L2
//! parity tests in `metriken-query-sql/src/live.rs::tests`).
//!
//! Running:
//!   cargo test -p dashboard --test sql_snapshots
//! Reviewing changes:
//!   cargo insta review --workspace
//! On first run every assertion creates a `.snap.new`; `cargo insta
//! accept` (or visual review via `cargo insta review`) turns those
//! into committed `.snap` files.

use dashboard::sql::{self, Arg, CgroupSide};

#[test]
fn rate_5m_total_emits_reset_aware_cte() {
    insta::assert_snapshot!(sql::rate_5m_total("^cpu_usage$"));
}

#[test]
fn irate_total_emits_unpivot_then_irate() {
    insta::assert_snapshot!(sql::irate_total("^cpu_usage/[a-z]+$"));
}

#[test]
fn irate_sum_by_id_groups_by_extracted_id() {
    insta::assert_snapshot!(sql::irate_sum_by_id(
        "^cpu_usage/[a-z]+/[0-9]+$",
        "/([0-9]+)$",
    ));
}

#[test]
fn irate_by_id_source_aware_variant() {
    insta::assert_snapshot!(sql::irate_by_id(
        "^cpu_usage/[a-z]+/[0-9]+$",
        "/([0-9]+)$",
    ));
}

#[test]
fn cpu_pct_total_divides_by_cpu_cores() {
    insta::assert_snapshot!(sql::cpu_pct_total("^cpu_usage/[a-z]+$"));
}

#[test]
fn cpu_pct_by_id_groups_per_cpu() {
    insta::assert_snapshot!(sql::cpu_pct_by_id(
        "^cpu_usage/[a-z]+/[0-9]+$",
        "/([0-9]+)$",
    ));
}

#[test]
fn hist_percentile_series_emits_h2_quantile_fanout() {
    insta::assert_snapshot!(sql::hist_percentile_series("syscall_latency"));
}

#[test]
fn concept_total_single_arg_sum() {
    insta::assert_snapshot!(sql::concept_total(
        "cpu_busy_pct",
        &[
            ("usage", Arg::Sum("^cpu_usage/[a-z]+$")),
            ("cores", Arg::Col("cpu_cores")),
        ],
    ));
}

#[test]
fn concept_total_multi_arg_mix() {
    // ipns(instructions, cycles, tsc, aperf, mperf, cores, ts) — 6 args
    insta::assert_snapshot!(sql::concept_total(
        "ipns",
        &[
            ("instructions", Arg::Sum("^cpu_instructions$")),
            ("cycles", Arg::Sum("^cpu_cycles$")),
            ("tsc", Arg::Sum("^cpu_tsc$")),
            ("aperf", Arg::Sum("^cpu_aperf$")),
            ("mperf", Arg::Sum("^cpu_mperf$")),
            ("cores", Arg::Col("cpu_cores")),
        ],
    ));
}

#[test]
fn ratio_by_id_emits_per_id_rate_pair() {
    insta::assert_snapshot!(sql::ratio_by_id(
        "^cpu_instructions/[0-9]+$",
        "^cpu_cycles/[0-9]+$",
        "/([0-9]+)$",
        "{n_rate} / NULLIF({d_rate}, 0)",
    ));
}

#[test]
fn cgroup_irate_total_aggregate_side() {
    insta::assert_snapshot!(sql::cgroup_irate_total(
        "cgroup_cpu_usage",
        CgroupSide::Aggregate,
        None,
    ));
}

#[test]
fn cgroup_irate_total_individual_with_label_filter() {
    insta::assert_snapshot!(sql::cgroup_irate_total(
        "cgroup_cpu_usage",
        CgroupSide::Individual,
        Some(("state", "user")),
    ));
}

#[test]
fn cgroup_irate_by_name_per_cgroup_fanout() {
    insta::assert_snapshot!(sql::cgroup_irate_by_name(
        "cgroup_cpu_usage",
        CgroupSide::Individual,
        None,
    ));
}

#[test]
fn cgroup_ratio_total_two_metrics() {
    insta::assert_snapshot!(sql::cgroup_ratio_total(
        "cgroup_cpu_instructions",
        "cgroup_cpu_cycles",
        CgroupSide::Aggregate,
    ));
}

#[test]
fn cgroup_ratio_by_name_two_metrics() {
    insta::assert_snapshot!(sql::cgroup_ratio_by_name(
        "cgroup_cpu_instructions",
        "cgroup_cpu_cycles",
        CgroupSide::Individual,
    ));
}

#[test]
fn nway_ratio_by_id_three_inputs() {
    // ipns-per-cpu: ((i/c) × (t·a/m) / 1e9) — 5 inputs in practice;
    // 3-arg case exercises the same code path with less noise.
    insta::assert_snapshot!(sql::nway_ratio_by_id(
        &[
            ("i", "^cpu_instructions/[0-9]+$"),
            ("c", "^cpu_cycles/[0-9]+$"),
            ("t", "^cpu_tsc/[0-9]+$"),
        ],
        "/([0-9]+)$",
        "(i_rate / NULLIF(c_rate, 0)) * (t_rate)",
    ));
}

#[test]
fn scale_v_wraps_inner_in_division() {
    insta::assert_snapshot!(sql::scale_v(
        "SELECT timestamp::DOUBLE/1e9 AS t, 42::DOUBLE AS v FROM _src".to_string(),
        1e9,
    ));
}

#[test]
fn bucket_heatmap_sql_default_view() {
    insta::assert_snapshot!(sql::bucket_heatmap_sql("syscall_latency", None));
}

#[test]
fn bucket_heatmap_sql_per_source_view() {
    insta::assert_snapshot!(sql::bucket_heatmap_sql(
        "response_latency",
        Some("cachecannon"),
    ));
}

#[test]
fn quantile_spectrum_sql_default_view() {
    insta::assert_snapshot!(sql::quantile_spectrum_sql(
        "syscall_latency",
        &[0.0, 0.5, 0.9, 0.99, 0.999, 0.9999, 1.0],
        7,
        None,
    ));
}

#[test]
fn quantile_spectrum_sql_per_source_view() {
    insta::assert_snapshot!(sql::quantile_spectrum_sql(
        "response_latency",
        &[0.0, 0.5, 0.9, 0.99],
        3,
        Some("cachecannon"),
    ));
}

#[test]
fn hist_percentile_series_combined_uses_h2_combine() {
    insta::assert_snapshot!(sql::hist_percentile_series_combined(
        "^syscall_latency/[a-z]+:buckets$",
    ));
}
