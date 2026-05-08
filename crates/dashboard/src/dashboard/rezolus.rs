use crate::data::DashboardData;
use crate::plot::*;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Rezolus
     */

    let mut rezolus = Group::new("Rezolus", "rezolus");

    let resources = rezolus.subgroup("Resource Usage");
    resources.describe("CPU and memory consumed by the Rezolus agent itself.");
    resources.plot_promql_with_sql(
        PlotOpts::counter("CPU %", "cpu", Unit::Percentage).percentage_range(),
        "sum(irate(rezolus_cpu_usage[5m])) / 1000000000".to_string(),
        // rezolus_cpu_usage is split per state (user/system) on demo —
        // sum across all matching columns first, then per-second irate,
        // then divide by 1e9 (PromQL counter is in nanoseconds).
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^rezolus_cpu_usage(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) / 1e9 AS v FROM agg"#.to_string(),
    );
    resources.plot_promql_with_sql(
        PlotOpts::gauge("Memory (RSS)", "memory", Unit::Bytes),
        "sum(rezolus_memory_usage_resident_set_size)".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  list_sum([*COLUMNS('^rezolus_memory_usage_resident_set_size(/[^:]+)?$')]::BIGINT[])::DOUBLE AS v
           FROM _src"#.to_string(),
    );

    let perf = rezolus.subgroup("Performance");
    perf.describe("Rezolus's own IPC and syscall rate, measured via the rezolus.service cgroup.");
    perf.plot_promql_with_sql(
        PlotOpts::counter("IPC", "ipc", Unit::Count),
        "sum(irate(cgroup_cpu_instructions{name=\"/system.slice/rezolus.service\"}[5m])) / sum(irate(cgroup_cpu_cycles{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
        // The cgroup name "/system.slice/rezolus.service" maps to physical
        // columns of the form cgroup_cpu_<metric>/system.slice/rezolus.service/<id>.
        // (Leading slash on the cgroup path is absorbed into the metric/path
        // boundary in metriken-exposition's wide-form encoding.)
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^cgroup_cpu_instructions/system.slice/rezolus.service/[0-9]+$')]::UBIGINT[]) AS instr,
                     list_sum([*COLUMNS('^cgroup_cpu_cycles/system.slice/rezolus.service/[0-9]+$')]::UBIGINT[]) AS cyc
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, ipc(instr, cyc, timestamp) AS v FROM agg"#.to_string(),
    );
    perf.plot_promql_with_sql(
        PlotOpts::counter("Syscalls", "syscalls", Unit::Rate),
        "sum(irate(cgroup_syscall{name=\"/system.slice/rezolus.service\"}[5m]))".to_string(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^cgroup_syscall/system.slice/rezolus.service/[0-9]+$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) AS v FROM agg"#.to_string(),
    );

    let bpf = rezolus.subgroup("BPF Overhead");
    bpf.describe("Time spent in BPF programs — total agent overhead and per-sampler breakdown.");
    bpf.plot_promql_with_sql_full(
        PlotOpts::counter("Total BPF Overhead", "bpf-overhead", Unit::Count),
        "sum(irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^rezolus_bpf_run_time(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(s, timestamp) / 1e9 AS v FROM agg"#.to_string(),
    );
    bpf.plot_promql_with_sql(
        PlotOpts::counter(
            "BPF Per-Sampler Overhead",
            "bpf-sampler-overhead",
            Unit::Count,
        ),
        "sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / 1000000000".to_string(),
        // Per-sampler fan-out via UNPIVOT. Column convention is
        // rezolus_bpf_run_time/<sampler>; we extract the sampler name
        // from the column with regexp_extract.
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^rezolus_bpf_run_time/[^/]+$') FROM _src)
                  ON COLUMNS('^rezolus_bpf_run_time/[^/]+$')
                  INTO NAME col VALUE v
           )
           SELECT timestamp::DOUBLE/1e9 AS t,
                  regexp_extract(col, '^rezolus_bpf_run_time/(.+)$', 1) AS sampler,
                  irate_lag(
                      v,
                      LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                  ) / 1e9 AS v
           FROM unp"#.to_string(),
    );
    bpf.plot_promql_with_sql(
        PlotOpts::counter(
            "BPF Per-Sampler Execution Time",
            "bpf-execution-time",
            Unit::Time,
        ),
        "(sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / sum by (sampler) (irate(rezolus_bpf_run_count[5m]))) / 1000000000".to_string(),
        // Per-sampler ratio: irate(time) / irate(count), then ns→s.
        // We zip the two metrics' columns in a UNION ALL UNPIVOT so each
        // (sampler, timestamp) row carries both numerator and denominator.
        // Easier: do two separate UNPIVOTs and join on (sampler, timestamp).
        r#"WITH t_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^rezolus_bpf_run_time/[^/]+$') FROM _src)
                  ON COLUMNS('^rezolus_bpf_run_time/[^/]+$')
                  INTO NAME col VALUE v
           ),
           c_unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^rezolus_bpf_run_count/[^/]+$') FROM _src)
                  ON COLUMNS('^rezolus_bpf_run_count/[^/]+$')
                  INTO NAME col VALUE v
           ),
           t_rates AS (
              SELECT timestamp,
                     regexp_extract(col, '^rezolus_bpf_run_time/(.+)$', 1) AS sampler,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS r
              FROM t_unp
           ),
           c_rates AS (
              SELECT timestamp,
                     regexp_extract(col, '^rezolus_bpf_run_count/(.+)$', 1) AS sampler,
                     irate_lag(
                         v,
                         LAG(v) OVER (PARTITION BY col ORDER BY timestamp),
                         timestamp - LAG(timestamp) OVER (PARTITION BY col ORDER BY timestamp)
                     ) AS r
              FROM c_unp
           )
           SELECT t.timestamp::DOUBLE/1e9 AS t,
                  t.sampler AS sampler,
                  (t.r / NULLIF(c.r, 0)) / 1e9 AS v
           FROM t_rates t JOIN c_rates c
               ON t.timestamp = c.timestamp AND t.sampler = c.sampler"#.to_string(),
    );

    view.group(rezolus);

    view
}
