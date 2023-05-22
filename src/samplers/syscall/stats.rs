use crate::*;

counter_with_heatmap!(
    SYSCALL_TOTAL,
    SYSCALL_TOTAL_HEATMAP,
    "syscall/total",
    "tracks total syscalls"
);
counter_with_heatmap!(
    SYSCALL_READ,
    SYSCALL_READ_HEATMAP,
    "syscall/read",
    "tracks read related syscalls"
);
counter_with_heatmap!(
    SYSCALL_WRITE,
    SYSCALL_WRITE_HEATMAP,
    "syscall/write",
    "tracks write related syscalls"
);
heatmap!(
    SYSCALL_TOTAL_LATENCY,
    "syscall/total/latency",
    "latency of all syscalls"
);
