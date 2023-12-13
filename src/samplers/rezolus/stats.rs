use crate::*;

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
counter!(RU_MINFLT, "rezolus/memory/page/reclaims");
counter!(RU_MAJFLT, "rezolus/memory/page/faults");
counter!(RU_NSWAP, "rezolus/memory/swapped");

counter!(RU_INBLOCK, "rezolus/io/block/reads");
counter!(RU_OUBLOCK, "rezolus/io/block/writes");

counter!(RU_MSGSND, "rezolus/messages/sentg");
counter!(RU_MSGRCV, "rezolus/messages/received");

counter!(RU_NSIGNALS, "rezolus/signals/received");

counter!(RU_NVCSW, "rezolus/context_switch/voluntary");
counter!(RU_NIVCSW, "rezolus/context_switch/involuntary");
