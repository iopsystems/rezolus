use crate::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

heatmap!(SCHEDULER_RUNQUEUE_LATENCY, "scheduler/runqueue/latency", "distribution of task wait times in the runqueue");
