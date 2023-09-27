use crate::*;

bpfhistogram!(
    SCHEDULER_RUNQUEUE_LATENCY,
    "scheduler/runqueue/latency",
    "distribution of task wait times in the runqueue"
);
bpfhistogram!(
    SCHEDULER_RUNNING,
    "scheduler/running",
    "distribution of task on-cpu time"
);
counter!(
    SCHEDULER_IVCSW,
    "scheduler/context_switch/involuntary",
    "count of involuntary context switches"
);
