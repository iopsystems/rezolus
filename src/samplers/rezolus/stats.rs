use crate::*;
use metriken::metric;
use metriken::Counter;
use metriken::Gauge;

counter_with_histogram!(
    RU_UTIME,
    RU_UTIME_HISTOGRAM,
    "rezolus/cpu/usage/user",
    "The amount of CPU time Rezolus was executing in user mode"
);
counter_with_histogram!(
    RU_STIME,
    RU_STIME_HISTOGRAM,
    "rezolus/cpu/usage/system",
    "The amount of CPU time Rezolus was executing in system mode"
);

gauge!(
    RU_MAXRSS,
    "rezolus/memory/usage/resident_set_size",
    "The total amount of memory allocated by Rezolus"
);
gauge!(RU_IXRSS, "rezolus/memory/usage/shared_memory_size");
gauge!(RU_IDRSS, "rezolus/memory/usage/data_size");
gauge!(RU_ISRSS, "rezolus/memory/usage/stack_size");
gauge!(RU_MINFLT, "rezolus/memory/page/reclaims");
gauge!(RU_MAJFLT, "rezolus/memory/page/faults");
gauge!(RU_NSWAP, "rezolus/memory/usage/shared_memory_size");

gauge!(RU_INBLOCK, "rezolus/io/block/reads");
gauge!(RU_OUBLOCK, "rezolus/io/block/writes");

gauge!(RU_MSGSND, "rezolus/messages/sentg");
gauge!(RU_MSGRCV, "rezolus/messages/received");

gauge!(RU_NSIGNALS, "rezolus/signals/received");

gauge!(RU_NVCSW, "rezolus/context_switch/voluntary");
gauge!(RU_NIVCSW, "rezolus/context_switch/involuntary");
