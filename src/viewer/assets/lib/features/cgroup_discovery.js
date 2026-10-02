// Which cgroups a recording has, from the labels of its per-cgroup series.
//
// `cgroup_cpu_usage` is tried first, as it always was: when present it covers
// every cgroup that ran. Every per-cgroup series can now be switched off per
// sampler (`cgroup_attribution`), so a recording can lack it while other
// samplers still carry per-cgroup series. Then the names come from the union
// of those, so the selector does not report "no cgroup data" for a recording
// that has some.

/** The per-cgroup series tried after `cgroup_cpu_usage`, by sampler. */
export const FALLBACK_CGROUP_METRICS = [
    'cgroup_cpu_cycles',
    'cgroup_cpu_instructions',
    'cgroup_cpu_migrations',
    'cgroup_cpu_tlb_flush',
    'cgroup_cpu_throttled',
    'cgroup_cpu_throttled_time',
    'cgroup_scheduler_runqueue_wait',
    'cgroup_scheduler_context_switch',
    'cgroup_syscall',
];

const PRIMARY_QUERIES = [
    'sum by (name) (cgroup_cpu_usage)',
    'group by (name) (cgroup_cpu_usage)',
    'cgroup_cpu_usage',
    'sum by (name) (rate(cgroup_cpu_usage[1m]))',
];

/** Extract cgroup names from a PromQL query result's metric labels. */
export const extractCgroupNames = (result) => {
    const names = new Set();
    if (result?.status !== 'success' || !result.data?.result?.length) return names;

    for (const series of result.data.result) {
        if (!series.metric) continue;
        for (const [key, value] of Object.entries(series.metric)) {
            if ((key === 'name' || key.includes('cgroup') || key === 'container') && value) {
                names.add(value);
            }
        }
    }
    return names;
};

/**
 * The cgroup names in a recording. `executeQuery(query)` resolves to a
 * PromQL result; a query that throws is skipped, as a metric the recording
 * lacks may.
 */
export const discoverCgroups = async (executeQuery) => {
    const run = async (query) => {
        try {
            return extractCgroupNames(await executeQuery(query));
        } catch (e) {
            console.warn(`Query failed: ${query}`, e);
            return new Set();
        }
    };

    for (const query of PRIMARY_QUERIES) {
        const names = await run(query);
        if (names.size > 0) return names;
    }

    const union = new Set();
    for (const metric of FALLBACK_CGROUP_METRICS) {
        for (const name of await run(`group by (name) (${metric})`)) {
            union.add(name);
        }
    }
    return union;
};
