use crate::data::DashboardData;
use crate::plot::*;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Usage
     */

    let mut usage = Group::new("Usage", "usage");

    let capacity = usage.subgroup("Capacity");
    capacity.describe("How much memory exists and how much of it is unclaimed.");
    capacity.plot_promql_with_sql(
        PlotOpts::gauge("Total", "total", Unit::Bytes),
        "memory_total".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t, "memory_total"::DOUBLE AS v FROM _src"#.to_string(),
    );
    capacity.plot_promql_with_sql(
        PlotOpts::gauge("Available", "available", Unit::Bytes),
        "memory_available".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t, "memory_available"::DOUBLE AS v FROM _src"#.to_string(),
    );
    capacity.plot_promql_with_sql(
        PlotOpts::gauge("Free", "free", Unit::Bytes),
        "memory_free".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t, "memory_free"::DOUBLE AS v FROM _src"#.to_string(),
    );

    let breakdown = usage.subgroup("Breakdown");
    breakdown.describe("Where allocated memory is going — kernel buffers, page cache, anonymous use — with overall utilization.");
    breakdown.plot_promql_with_sql(
        PlotOpts::gauge("Buffers", "buffers", Unit::Bytes),
        "memory_buffers".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t, "memory_buffers"::DOUBLE AS v FROM _src"#.to_string(),
    );
    breakdown.plot_promql_with_sql(
        PlotOpts::gauge("Cached", "cached", Unit::Bytes),
        "memory_cached".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t, "memory_cached"::DOUBLE AS v FROM _src"#.to_string(),
    );
    breakdown.plot_promql_with_sql(
        PlotOpts::gauge("Used", "used", Unit::Bytes),
        "memory_total - memory_available".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  ("memory_total" - "memory_available")::DOUBLE AS v
           FROM _src"#.to_string(),
    );
    breakdown.plot_promql_with_sql(
        PlotOpts::gauge("Utilization %", "utilization-pct", Unit::Percentage).percentage_range(),
        "(memory_total - memory_available) / memory_total".to_string(),
        r#"SELECT timestamp::DOUBLE/1e9 AS t,
                  ("memory_total" - "memory_available")::DOUBLE / NULLIF("memory_total"::DOUBLE, 0) AS v
           FROM _src"#.to_string(),
    );

    view.group(usage);

    /*
     * NUMA
     */

    let mut numa = Group::new("NUMA", "numa");

    let locality = numa.subgroup("Local vs Remote");
    locality.describe("Local allocations hit node-local RAM (fast); remote allocations cross the interconnect (slow).");
    // PromQL `rate(M[5m])` produces one series per matching label set;
    // dashboards display it as a single line, so for the SQL form we sum
    // across any per-NUMA-node columns first, then take the 5m windowed
    // rate via the rate_5m macro. Anchored regex per the convention in
    // crates/viewer-sql/duckdb.md (substring matching footgun).
    locality.plot_promql_with_sql(
        PlotOpts::counter("Local Rate", "numa-local-rate", Unit::Rate),
        "rate(memory_numa_local[5m])".to_string(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^memory_numa_local(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, rate_5m(s, timestamp) AS v FROM agg"#.to_string(),
    );
    locality.plot_promql_with_sql(
        PlotOpts::counter("Remote Rate", "numa-remote-rate", Unit::Rate),
        "rate(memory_numa_foreign[5m])".to_string(),
        r#"WITH agg AS (
              SELECT timestamp,
                     list_sum([*COLUMNS('^memory_numa_foreign(/[^:]+)?$')]::UBIGINT[]) AS s
              FROM _src
           )
           SELECT timestamp::DOUBLE/1e9 AS t, rate_5m(s, timestamp) AS v FROM agg"#.to_string(),
    );

    view.group(numa);

    view
}
