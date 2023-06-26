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

counter!(CPU_CYCLES, "cpu/cycles");
counter!(CPU_INSTRUCTIONS, "cpu/instructions");
gauge!(IPKC, "cpu/ipkc", "Average Instructions Per Thousand Cycle");
gauge!(
    IPKNS,
    "cpu/ipkns",
    "Average Instructions Per Thousand Nanosecond"
);
gauge!(
    BASEFREQUENCY,
    "cpu/base_frequency",
    "The base CPU frequency"
);
gauge!(
    RUNNINGFREQUENCY,
    "cpu/running_frequency",
    "Average running CPU frequency"
);
