use crate::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

heatmap!(BLOCKIO_LATENCY, "blockio/latency", "distribution of block IO latencies");
// heatmap!(SCHEDULER_RUNNING, "scheduler/running", "distribution of task on-cpu time");
// counter!(SCHEDULER_IVCSW, "scheduler/context_switch/involuntary", "count of involuntary context switches");
// counter!(SCHEDULER_VCSW, "scheduler/context_switch/voluntary", "count of voluntary context switches");
