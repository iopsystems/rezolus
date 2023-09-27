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
    "tracks read related syscalls (read, recvfrom, ...)"
);
counter_with_heatmap!(
    SYSCALL_WRITE,
    SYSCALL_WRITE_HEATMAP,
    "syscall/write",
    "tracks write related syscalls (write, sendto, ...)"
);
counter_with_heatmap!(
    SYSCALL_POLL,
    SYSCALL_POLL_HEATMAP,
    "syscall/poll",
    "tracks poll related syscalls (poll, select, epoll, ...)"
);
counter_with_heatmap!(
    SYSCALL_LOCK,
    SYSCALL_LOCK_HEATMAP,
    "syscall/lock",
    "tracks lock related syscalls (futex)"
);
counter_with_heatmap!(
    SYSCALL_TIME,
    SYSCALL_TIME_HEATMAP,
    "syscall/time",
    "tracks time related syscalls (clock_gettime, clock_settime, clock_getres, ...)"
);
counter_with_heatmap!(
    SYSCALL_SLEEP,
    SYSCALL_SLEEP_HEATMAP,
    "syscall/sleep",
    "tracks sleep related syscalls (nanosleep, clock_nanosleep)"
);
counter_with_heatmap!(
    SYSCALL_SOCKET,
    SYSCALL_SOCKET_HEATMAP,
    "syscall/socket",
    "tracks socket related syscalls (accept, connect, bind, setsockopt, ...)"
);
bpfhistogram!(
    SYSCALL_TOTAL_LATENCY,
    "syscall/total/latency",
    "latency of all syscalls"
);
