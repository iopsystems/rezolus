use crate::*;

counter_with_heatmap!(CPU_USAGE_USER, CPU_USAGE_USER_HEATMAP, "cpu/usage/user");
counter_with_heatmap!(CPU_USAGE_NICE, CPU_USAGE_NICE_HEATMAP, "cpu/usage/nice");
counter_with_heatmap!(
    CPU_USAGE_SYSTEM,
    CPU_USAGE_SYSTEM_HEATMAP,
    "cpu/usage/system"
);
counter_with_heatmap!(CPU_USAGE_IDLE, CPU_USAGE_IDLE_HEATMAP, "cpu/usage/idle");
counter_with_heatmap!(
    CPU_USAGE_IO_WAIT,
    CPU_USAGE_IO_WAIT_HEATMAP,
    "cpu/usage/io_wait"
);
counter_with_heatmap!(CPU_USAGE_IRQ, CPU_USAGE_IRQ_HEATMAP, "cpu/usage/irq");
counter_with_heatmap!(
    CPU_USAGE_SOFTIRQ,
    CPU_USAGE_SOFTIRQ_HEATMAP,
    "cpu/usage/softirq"
);
counter_with_heatmap!(CPU_USAGE_STEAL, CPU_USAGE_STEAL_HEATMAP, "cpu/usage/steal");
counter_with_heatmap!(CPU_USAGE_GUEST, CPU_USAGE_GUEST_HEATMAP, "cpu/usage/guest");
counter_with_heatmap!(
    CPU_USAGE_GUEST_NICE,
    CPU_USAGE_GUEST_NICE_HEATMAP,
    "cpu/usage/guest_nice"
);

heatmap!(
    CPU_FREQUENCY,
    "cpu/frequency",
    "distribution of instantaneous CPU frequencies"
);

gauge!(
    CPU_CORES,
    "cpu/cores",
    "the count of logical cores that are online"
);

counter!(
    CPU_CYCLES,
    "cpu/cycles",
    "total executed CPU cycles on all CPUs"
);
counter!(
    CPU_INSTRUCTIONS,
    "cpu/instructions",
    "total retired instructions on all CPUs"
);

gauge!(
    CPU_ACTIVE_PERF_GROUPS,
    "cpu/active_perf_groups",
    "The number of active perf groups"
);

gauge!(
    CPU_AVG_IPKC,
    "cpu/avg_ipkc",
    "average Instructions Per Thousand Cycles: SUM(IPKC_CPU0...N)/N)"
);

heatmap!(
    CPU_IPKC,
    "cpu/ipkc",
    "distribution of per-CPU IPKC (Instructions Per Thousand Cycles)"
);

gauge!(
    CPU_AVG_IPUS,
    "cpu/avg_ipus",
    "Average Instructions Per Microsecond: SUM(IPUS_CPU0...N)/N"
);

heatmap!(
    CPU_IPUS,
    "cpu/ipus",
    "distribution of per-CPU IPUS (Instructions Per Microsecond)"
);

gauge!(
    CPU_AVG_BASE_FREQUENCY,
    "cpu/base_frequency",
    "Average base CPU frequency"
);

heatmap!(
    CPU_RUNNING_FREQUENCY,
    "cpu/running_frequency",
    "distribution of the per-CPU running CPU frequency"
);

gauge!(
    CPU_AVG_RUNNING_FREQUENCY,
    "cpu/avg_running_frequency",
    "Average running CPU frequency: SUM(RUNNING_FREQUENCY_CPU0...N)/N"
);
