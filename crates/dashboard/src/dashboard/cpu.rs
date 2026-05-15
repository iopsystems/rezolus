use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;
use crate::sql::Arg;

const RATIO: &str = "{n} / NULLIF({d}, 0)";
const INVERSE_RATIO: &str = "1 - {n} / NULLIF({d}, 0)";

/// True iff the recording has more than one CPU. Per-core charts are
/// suppressed when this is false because they degenerate to the aggregate.
fn has_multiple_cpus(data: &dyn DashboardData) -> bool {
    [
        "cpu_usage",
        "cpu_cycles",
        "cpu_instructions",
        "cpu_tsc",
        "cpu_aperf",
        "cpu_mperf",
    ]
    .iter()
    .any(|m| data.unique_label_values(m, "id") > 1)
}

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let multi_cpu = has_multiple_cpus(data);

    /*
     * Utilization
     */

    let mut utilization = Group::new("Utilization", "utilization");

    let busy = utilization.subgroup("Total CPU");
    busy.describe("Overall CPU busy time across all cores, with per-core breakdown.");
    if multi_cpu {
        busy.plot_promql_with_sql(
            PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).percentage_range(),
            "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
            sql::concept_total(
                "cpu_busy_pct",
                &[
                    ("usage", Arg::Sum("^cpu_usage(/[^:]+)?$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
        busy.plot_promql_with_sql(
            PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage)
                .percentage_range(),
            "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
            // Per-CPU CPU% is just per-CPU rate / 1e9 (no cores divisor).
            sql::cpu_pct_by_id("^cpu_usage/[a-z]+/[0-9]+$", "/([0-9]+)$"),
        );
    } else {
        // Single-CPU: keep the aggregate chart but render full-width since
        // the per-CPU variant is suppressed.
        busy.plot_promql_with_sql_full(
            PlotOpts::counter("Busy %", "busy-pct", Unit::Percentage).percentage_range(),
            "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
            sql::concept_total(
                "cpu_busy_pct",
                &[
                    ("usage", Arg::Sum("^cpu_usage(/[^:]+)?$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
    }

    let by_state = utilization.subgroup("CPU Time by State");
    by_state.describe("Kernel vs. user-space CPU time, aggregate and per-core.");
    for state in &["user", "system"] {
        let capitalized = if *state == "user" { "User" } else { "System" };
        if multi_cpu {
            by_state.plot_promql_with_sql(
                PlotOpts::counter(
                    format!("{capitalized} %"),
                    format!("{state}-pct"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum(irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
                sql::concept_total(
                    "cpu_busy_pct",
                    &[
                        ("usage", Arg::Sum(&format!("^cpu_usage/{state}/[0-9]+$"))),
                        ("cores", Arg::Col("cpu_cores")),
                    ],
                ),
            );
            by_state.plot_promql_with_sql(
                PlotOpts::counter(
                    format!("{capitalized} % (Per-CPU)"),
                    format!("{state}-pct-per-cpu"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum by (id) (irate(cpu_usage{{state=\"{state}\"}}[5m])) / 1000000000"),
                sql::cpu_pct_by_id(&format!("^cpu_usage/{state}/[0-9]+$"), "/([0-9]+)$"),
            );
        } else {
            by_state.plot_promql_with_sql_full(
                PlotOpts::counter(
                    format!("{capitalized} %"),
                    format!("{state}-pct"),
                    Unit::Percentage,
                )
                .percentage_range(),
                format!("sum(irate(cpu_usage{{state=\"{state}\"}}[5m])) / cpu_cores / 1000000000"),
                sql::concept_total(
                    "cpu_busy_pct",
                    &[
                        ("usage", Arg::Sum(&format!("^cpu_usage/{state}/[0-9]+$"))),
                        ("cores", Arg::Col("cpu_cores")),
                    ],
                ),
            );
        }
    }

    view.group(utilization);

    /*
     * Performance
     */

    let mut performance = Group::new("Performance", "performance");

    let ipc = performance.subgroup("Instructions per Cycle");
    ipc.describe("How efficiently the CPU retires instructions per clock cycle.");
    if multi_cpu {
        ipc.plot_promql_with_sql(
            PlotOpts::counter("IPC", "ipc", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
            sql::concept_total(
                "ipc",
                &[
                    ("instr", Arg::Sum("^cpu_instructions/[0-9]+$")),
                    ("cyc", Arg::Sum("^cpu_cycles/[0-9]+$")),
                ],
            ),
        );
        ipc.plot_promql_with_sql(
            PlotOpts::counter("IPC (Per-CPU)", "ipc-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))"
                .to_string(),
            sql::ratio_by_id(
                "^cpu_instructions/[0-9]+$",
                "^cpu_cycles/[0-9]+$",
                "/([0-9]+)$",
                RATIO,
            ),
        );
    } else {
        ipc.plot_promql_with_sql_full(
            PlotOpts::counter("IPC", "ipc", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))".to_string(),
            sql::concept_total(
                "ipc",
                &[
                    ("instr", Arg::Sum("^cpu_instructions/[0-9]+$")),
                    ("cyc", Arg::Sum("^cpu_cycles/[0-9]+$")),
                ],
            ),
        );
    }

    let ipns = performance.subgroup("Instructions per Nanosecond");
    ipns.describe("Wall-clock-normalized instruction throughput — accounts for frequency scaling.");
    if multi_cpu {
        ipns.plot_promql_with_sql(
            PlotOpts::counter("IPNS", "ipns", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
            sql::concept_total(
                "ipns",
                &[
                    ("instr", Arg::Sum("^cpu_instructions/[0-9]+$")),
                    ("cyc",   Arg::Sum("^cpu_cycles/[0-9]+$")),
                    ("tsc",   Arg::Sum("^cpu_tsc/[0-9]+$")),
                    ("aperf", Arg::Sum("^cpu_aperf/[0-9]+$")),
                    ("mperf", Arg::Sum("^cpu_mperf/[0-9]+$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
        ipns.plot_promql_with_sql(
            PlotOpts::counter("IPNS (Per-CPU)", "ipns-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m])) * sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m])) / 1000000000".to_string(),
            // 5-way per-id join: UNPIVOT each metric on its `^cpu_<m>/[0-9]+$`
            // regex, join on (timestamp, id), compute per-id irate (PARTITION
            // BY id), then combine as (i/c) * (t*a/m) / 1e9 — the ipns formula.
            sql::nway_ratio_by_id(
                &[
                    ("i", "^cpu_instructions/[0-9]+$"),
                    ("c", "^cpu_cycles/[0-9]+$"),
                    ("t", "^cpu_tsc/[0-9]+$"),
                    ("a", "^cpu_aperf/[0-9]+$"),
                    ("m", "^cpu_mperf/[0-9]+$"),
                ],
                "/([0-9]+)$",
                "(i_rate / NULLIF(c_rate, 0)) * t_rate * a_rate / NULLIF(m_rate * 1000000000, 0)",
            ),
        );
    } else {
        ipns.plot_promql_with_sql_full(
            PlotOpts::counter("IPNS", "ipns", Unit::Count),
            "sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores".to_string(),
            sql::concept_total(
                "ipns",
                &[
                    ("instr", Arg::Sum("^cpu_instructions/[0-9]+$")),
                    ("cyc",   Arg::Sum("^cpu_cycles/[0-9]+$")),
                    ("tsc",   Arg::Sum("^cpu_tsc/[0-9]+$")),
                    ("aperf", Arg::Sum("^cpu_aperf/[0-9]+$")),
                    ("mperf", Arg::Sum("^cpu_mperf/[0-9]+$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
    }

    let l3 = performance.subgroup("L3 Cache Hit Rate");
    l3.describe("Fraction of L3 cache accesses that hit, indicating last-level cache efficiency.");
    if multi_cpu {
        l3.plot_promql_with_sql(
            PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).percentage_range(),
            "1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
            sql::concept_total(
                "l3_hit_pct",
                &[
                    ("miss", Arg::Sum("^cpu_l3_miss/[0-9]+$")),
                    ("access", Arg::Sum("^cpu_l3_access/[0-9]+$")),
                ],
            ),
        );
        l3.plot_promql_with_sql(
            PlotOpts::counter("L3 Hit % (Per-CPU)", "l3-hit-per-cpu", Unit::Percentage)
                .percentage_range(),
            "1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))"
                .to_string(),
            sql::ratio_by_id(
                "^cpu_l3_miss/[0-9]+$",
                "^cpu_l3_access/[0-9]+$",
                "/([0-9]+)$",
                INVERSE_RATIO,
            ),
        );
    } else {
        l3.plot_promql_with_sql_full(
            PlotOpts::counter("L3 Hit %", "l3-hit", Unit::Percentage).percentage_range(),
            "1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
            sql::concept_total(
                "l3_hit_pct",
                &[
                    ("miss", Arg::Sum("^cpu_l3_miss/[0-9]+$")),
                    ("access", Arg::Sum("^cpu_l3_access/[0-9]+$")),
                ],
            ),
        );
    }

    let freq = performance.subgroup("Frequency");
    freq.describe("Effective CPU clock speed, averaged and per-core.");
    if multi_cpu {
        freq.plot_promql_with_sql(
            PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
            "sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores".to_string(),
            sql::concept_total(
                "frequency_hz",
                &[
                    ("tsc",   Arg::Sum("^cpu_tsc/[0-9]+$")),
                    ("aperf", Arg::Sum("^cpu_aperf/[0-9]+$")),
                    ("mperf", Arg::Sum("^cpu_mperf/[0-9]+$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
        freq.plot_promql_with_sql(
            PlotOpts::counter("Frequency (Per-CPU)", "frequency-per-cpu", Unit::Frequency),
            "sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m]))".to_string(),
            // 3-way per-id join — formula tsc * aperf / mperf.
            sql::nway_ratio_by_id(
                &[
                    ("t", "^cpu_tsc/[0-9]+$"),
                    ("a", "^cpu_aperf/[0-9]+$"),
                    ("m", "^cpu_mperf/[0-9]+$"),
                ],
                "/([0-9]+)$",
                "t_rate * a_rate / NULLIF(m_rate, 0)",
            ),
        );
    } else {
        freq.plot_promql_with_sql_full(
            PlotOpts::counter("Frequency", "frequency", Unit::Frequency),
            "sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores".to_string(),
            sql::concept_total(
                "frequency_hz",
                &[
                    ("tsc",   Arg::Sum("^cpu_tsc/[0-9]+$")),
                    ("aperf", Arg::Sum("^cpu_aperf/[0-9]+$")),
                    ("mperf", Arg::Sum("^cpu_mperf/[0-9]+$")),
                    ("cores", Arg::Col("cpu_cores")),
                ],
            ),
        );
    }

    view.group(performance);

    /*
     * Branch Prediction
     */

    let mut branch = Group::new("Branch Prediction", "branch-prediction");

    let miss = branch.subgroup("Misprediction Rate");
    miss.describe("Fraction of branches that the predictor got wrong.");
    if multi_cpu {
        miss.plot_promql_with_sql(
            PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
                .percentage_range(),
            "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))"
                .to_string(),
            sql::concept_total(
                "branch_miss_pct",
                &[
                    ("misses", Arg::Sum("^cpu_branch_misses/[0-9]+$")),
                    ("branches", Arg::Sum("^cpu_branch_instructions/[0-9]+$")),
                ],
            ),
        );
        miss.plot_promql_with_sql(
            PlotOpts::counter(
                "Misprediction Rate % (Per-CPU)",
                "branch-miss-rate-per-cpu",
                Unit::Percentage,
            )
            .percentage_range(),
            "sum by (id) (irate(cpu_branch_misses[5m])) / sum by (id) (irate(cpu_branch_instructions[5m]))"
                .to_string(),
            sql::ratio_by_id(
                "^cpu_branch_misses/[0-9]+$",
                "^cpu_branch_instructions/[0-9]+$",
                "/([0-9]+)$",
                RATIO,
            ),
        );
    } else {
        miss.plot_promql_with_sql_full(
            PlotOpts::counter("Misprediction Rate %", "branch-miss-rate", Unit::Percentage)
                .percentage_range(),
            "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))"
                .to_string(),
            sql::concept_total(
                "branch_miss_pct",
                &[
                    ("misses", Arg::Sum("^cpu_branch_misses/[0-9]+$")),
                    ("branches", Arg::Sum("^cpu_branch_instructions/[0-9]+$")),
                ],
            ),
        );
    }

    let activity = branch.subgroup("Branch Activity");
    activity.describe("Absolute branch instruction and miss rates.");
    activity.plot_promql_with_sql(
        PlotOpts::counter("Instructions", "branch-instructions", Unit::Rate),
        "sum(irate(cpu_branch_instructions[5m]))".to_string(),
        sql::irate_total("^cpu_branch_instructions/[0-9]+$"),
    );
    if multi_cpu {
        activity.plot_promql_with_sql(
            PlotOpts::counter(
                "Instructions (Per-CPU)",
                "branch-instructions-per-cpu",
                Unit::Rate,
            ),
            "sum by (id) (irate(cpu_branch_instructions[5m]))".to_string(),
            sql::irate_by_id("^cpu_branch_instructions/[0-9]+$", "/([0-9]+)$"),
        );
    }
    activity.plot_promql_with_sql(
        PlotOpts::counter("Misses", "branch-misses", Unit::Rate),
        "sum(irate(cpu_branch_misses[5m]))".to_string(),
        sql::irate_total("^cpu_branch_misses/[0-9]+$"),
    );
    if multi_cpu {
        activity.plot_promql_with_sql(
            PlotOpts::counter("Misses (Per-CPU)", "branch-misses-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_branch_misses[5m]))".to_string(),
            sql::irate_by_id("^cpu_branch_misses/[0-9]+$", "/([0-9]+)$"),
        );
    }

    view.group(branch);

    /*
     * DTLB
     * cpu_dtlb_miss aggregates all variants:
     *   - unlabeled (AMD/ARM combined): cpu_dtlb_miss/<id>
     *   - op="load" (Intel): cpu_dtlb_miss/load/<id>
     *   - op="store" (Intel): cpu_dtlb_miss/store/<id>
     */

    let mut dtlb = Group::new("DTLB", "dtlb");

    let misses = dtlb.subgroup("DTLB Misses");
    misses.describe("Raw data-TLB miss rate, aggregated and per-core.");
    if multi_cpu {
        misses.plot_promql_with_sql(
            PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
            "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
            sql::irate_total("^cpu_dtlb_miss(/[^:]+)?$"),
        );
        misses.plot_promql_with_sql(
            PlotOpts::counter("Misses (Per-CPU)", "dtlb-misses-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_dtlb_miss[5m]))".to_string(),
            // Aggregate across op variants per id, then irate.
            // SUM(UBIGINT) promotes to HUGEINT in DuckDB; cast back so
            // `irate_lag`'s UBIGINT signature binds. cpu_dtlb_miss
            // counts per CPU fit u64 with extreme headroom.
            r#"WITH unp AS (
                  UNPIVOT (SELECT timestamp, COLUMNS('^cpu_dtlb_miss(/[^:]+)?$') FROM _src)
                      ON COLUMNS('^cpu_dtlb_miss(/[^:]+)?$') INTO NAME col VALUE v
               ),
               by_id AS (
                  SELECT timestamp,
                         regexp_extract(col, '/([0-9]+)$', 1) AS id,
                         CAST(SUM(v) AS UBIGINT) AS s
                  FROM unp
                  GROUP BY timestamp, id
               )
               SELECT timestamp::DOUBLE/1e9 AS t, id,
                      irate_lag(
                          s,
                          LAG(s) OVER (PARTITION BY id ORDER BY timestamp),
                          timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)
                      ) AS v
               FROM by_id"#
                .to_string(),
        );
    } else {
        misses.plot_promql_with_sql_full(
            PlotOpts::counter("Misses", "dtlb-misses", Unit::Rate),
            "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
            sql::irate_total("^cpu_dtlb_miss(/[^:]+)?$"),
        );
    }

    let mpki = dtlb.subgroup("DTLB MPKI");
    mpki.describe("Misses per thousand instructions, normalized so workload differences don't distort the rate.");
    if multi_cpu {
        mpki.plot_promql_with_sql(
            PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
            "sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000".to_string(),
            sql::concept_total(
                "dtlb_mpki",
                &[
                    ("misses", Arg::Sum("^cpu_dtlb_miss(/[^:]+)?$")),
                    ("instructions", Arg::Sum("^cpu_instructions/[0-9]+$")),
                ],
            ),
        );
        mpki.plot_promql_with_sql(
            PlotOpts::counter("MPKI (Per-CPU)", "dtlb-mpki-per-cpu", Unit::Count),
            "sum by (id) (irate(cpu_dtlb_miss[5m])) / sum by (id) (irate(cpu_instructions[5m])) * 1000"
                .to_string(),
            // dtlb_miss has variant-suffixed columns; collapse per id, then
            // build per-id rates with PARTITION BY id (the reusable
            // `ratio_by_id` helper assumes 1:1 metric-to-id mapping, which
            // doesn't hold when miss has 3 variant columns per id), then ratio.
            r#"WITH miss_unp AS (
                  UNPIVOT (SELECT timestamp, COLUMNS('^cpu_dtlb_miss(/[^:]+)?$') FROM _src)
                      ON COLUMNS('^cpu_dtlb_miss(/[^:]+)?$') INTO NAME col VALUE v
               ),
               miss_by_id AS (
                  -- CAST(SUM(v) AS UBIGINT): DuckDB promotes
                  -- SUM(UBIGINT) → HUGEINT, but `irate_lag` is
                  -- UBIGINT-typed. Per-CPU miss counts fit u64.
                  SELECT timestamp, regexp_extract(col, '/([0-9]+)$', 1) AS id,
                         CAST(SUM(v) AS UBIGINT) AS s
                  FROM miss_unp GROUP BY timestamp, id
               ),
               instr_unp AS (
                  UNPIVOT (SELECT timestamp, COLUMNS('^cpu_instructions/[0-9]+$') FROM _src)
                      ON COLUMNS('^cpu_instructions/[0-9]+$') INTO NAME col VALUE v
               ),
               instr_by_id AS (
                  SELECT timestamp, regexp_extract(col, '/([0-9]+)$', 1) AS id, v
                  FROM instr_unp
               ),
               joined AS (
                  SELECT m.timestamp, m.id, m.s AS miss_v, i.v AS instr_v
                  FROM miss_by_id m JOIN instr_by_id i
                      ON m.timestamp = i.timestamp AND m.id = i.id
               ),
               rates AS (
                  SELECT timestamp, id,
                         irate_lag(miss_v,
                                   LAG(miss_v) OVER (PARTITION BY id ORDER BY timestamp),
                                   timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)) AS m_rate,
                         irate_lag(instr_v,
                                   LAG(instr_v) OVER (PARTITION BY id ORDER BY timestamp),
                                   timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)) AS i_rate
                  FROM joined
               )
               SELECT timestamp::DOUBLE/1e9 AS t, id,
                      m_rate / NULLIF(i_rate, 0) * 1000 AS v
               FROM rates"#.to_string(),
        );
    } else {
        mpki.plot_promql_with_sql_full(
            PlotOpts::counter("MPKI", "dtlb-mpki", Unit::Count),
            "sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000".to_string(),
            sql::concept_total(
                "dtlb_mpki",
                &[
                    ("misses", Arg::Sum("^cpu_dtlb_miss(/[^:]+)?$")),
                    ("instructions", Arg::Sum("^cpu_instructions/[0-9]+$")),
                ],
            ),
        );
    }

    view.group(dtlb);

    /*
     * Migrations
     */

    let mut migrations = Group::new("Migrations", "migrations");

    let to = migrations.subgroup("Incoming Migrations");
    to.describe("Tasks migrated onto a CPU, per second.");
    if multi_cpu {
        to.plot_promql_with_sql(
            PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
            sql::irate_total("^cpu_migrations/to/[0-9]+$"),
        );
        to.plot_promql_with_sql(
            PlotOpts::counter("To (Per-CPU)", "cpu-migrations-to-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
            sql::irate_by_id("^cpu_migrations/to/[0-9]+$", "/([0-9]+)$"),
        );
    } else {
        to.plot_promql_with_sql_full(
            PlotOpts::counter("To", "cpu-migrations-to", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"to\"}[5m]))".to_string(),
            sql::irate_total("^cpu_migrations/to/[0-9]+$"),
        );
    }

    let from = migrations.subgroup("Outgoing Migrations");
    from.describe("Tasks migrated off a CPU, per second.");
    if multi_cpu {
        from.plot_promql_with_sql(
            PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
            sql::irate_total("^cpu_migrations/from/[0-9]+$"),
        );
        from.plot_promql_with_sql(
            PlotOpts::counter("From (Per-CPU)", "cpu-migrations-from-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
            sql::irate_by_id("^cpu_migrations/from/[0-9]+$", "/([0-9]+)$"),
        );
    } else {
        from.plot_promql_with_sql_full(
            PlotOpts::counter("From", "cpu-migrations-from", Unit::Rate),
            "sum(irate(cpu_migrations{direction=\"from\"}[5m]))".to_string(),
            sql::irate_total("^cpu_migrations/from/[0-9]+$"),
        );
    }

    view.group(migrations);

    /*
     * TLB Flush
     */

    let mut tlb = Group::new("TLB Flush", "tlb-flush");

    let total = tlb.subgroup("Total TLB Flushes");
    total.describe("Aggregate TLB invalidation rate across all reasons.");
    if multi_cpu {
        total.plot_promql_with_sql(
            PlotOpts::counter("Total", "tlb-total", Unit::Rate),
            "sum(irate(cpu_tlb_flush[5m]))".to_string(),
            sql::irate_total("^cpu_tlb_flush(/[^:]+)?$"),
        );
        total.plot_promql_with_sql(
            PlotOpts::counter("Total (Per-CPU)", "tlb-total-per-cpu", Unit::Rate),
            "sum by (id) (irate(cpu_tlb_flush[5m]))".to_string(),
            sql::irate_sum_by_id("^cpu_tlb_flush(/[^:]+)?$", "/([0-9]+)$"),
        );
    } else {
        total.plot_promql_with_sql_full(
            PlotOpts::counter("Total", "tlb-total", Unit::Rate),
            "sum(irate(cpu_tlb_flush[5m]))".to_string(),
            sql::irate_total("^cpu_tlb_flush(/[^:]+)?$"),
        );
    }

    for reason in &[
        ("local_mm_shootdown", "Local MM Shootdown"),
        ("remote_send_ipi", "Remote Send IPI"),
        ("remote_shootdown", "Remote Shootdown"),
        ("task_switch", "Task Switch"),
    ] {
        let (reason_value, label) = reason;
        let id = format!("tlb-{}", reason_value.replace('_', "-"));
        let sg = tlb.subgroup(*label);
        if multi_cpu {
            sg.plot_promql_with_sql(
                PlotOpts::counter(*label, &id, Unit::Rate),
                format!("sum(irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
                sql::irate_total(&format!("^cpu_tlb_flush/{reason_value}/[0-9]+$")),
            );
            sg.plot_promql_with_sql(
                PlotOpts::counter(
                    format!("{label} (Per-CPU)"),
                    format!("{id}-per-cpu"),
                    Unit::Rate,
                ),
                format!("sum by (id) (irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
                sql::irate_by_id(
                    &format!("^cpu_tlb_flush/{reason_value}/[0-9]+$"),
                    "/([0-9]+)$",
                ),
            );
        } else {
            sg.plot_promql_with_sql_full(
                PlotOpts::counter(*label, &id, Unit::Rate),
                format!("sum(irate(cpu_tlb_flush{{reason=\"{reason_value}\"}}[5m]))"),
                sql::irate_total(&format!("^cpu_tlb_flush/{reason_value}/[0-9]+$")),
            );
        }
    }

    view.group(tlb);

    view
}
