use crate::*;

counter_with_histogram!(
    SYSCALL_TOTAL,
    SYSCALL_TOTAL_HISTOGRAM,
    "syscall/total",
    "tracks total syscalls"
);
counter_with_histogram!(
    SYSCALL_READ,
    SYSCALL_READ_HISTOGRAM,
    "syscall/read",
    "tracks read related syscalls (read, recvfrom, ...)"
);
counter_with_histogram!(
    SYSCALL_WRITE,
    SYSCALL_WRITE_HISTOGRAM,
    "syscall/write",
    "tracks write related syscalls (write, sendto, ...)"
);
counter_with_histogram!(
    SYSCALL_POLL,
    SYSCALL_POLL_HISTOGRAM,
    "syscall/poll",
    "tracks poll related syscalls (poll, select, epoll, ...)"
);
counter_with_histogram!(
    SYSCALL_LOCK,
    SYSCALL_LOCK_HISTOGRAM,
    "syscall/lock",
    "tracks lock related syscalls (futex)"
);
counter_with_histogram!(
    SYSCALL_TIME,
    SYSCALL_TIME_HISTOGRAM,
    "syscall/time",
    "tracks time related syscalls (clock_gettime, clock_settime, clock_getres, ...)"
);
counter_with_histogram!(
    SYSCALL_SLEEP,
    SYSCALL_SLEEP_HISTOGRAM,
    "syscall/sleep",
    "tracks sleep related syscalls (nanosleep, clock_nanosleep)"
);
counter_with_histogram!(
    SYSCALL_SOCKET,
    SYSCALL_SOCKET_HISTOGRAM,
    "syscall/socket",
    "tracks socket related syscalls (accept, connect, bind, setsockopt, ...)"
);
bpfhistogram!(
    SYSCALL_TOTAL_LATENCY,
    "syscall/total/latency",
    "latency of all syscalls"
);
bpfhistogram!(
    SYSCALL_READ_LATENCY,
    "syscall/read/latency",
    "latency of read related syscalls (read, recvfrom, ...)"
);
bpfhistogram!(
    SYSCALL_WRITE_LATENCY,
    "syscall/write/latency",
    "latency of write related syscalls (write, sendto, ...)"
);
bpfhistogram!(
    SYSCALL_POLL_LATENCY,
    "syscall/poll/latency",
    "latency of poll related syscalls (poll, select, epoll, ...)"
);
bpfhistogram!(
    SYSCALL_LOCK_LATENCY,
    "syscall/lock/latency",
    "latency of lock related syscalls (futex)"
);
bpfhistogram!(
    SYSCALL_TIME_LATENCY,
    "syscall/time/latency",
    "latency of time related syscalls (clock_gettime, clock_settime, clock_getres, ...)"
);
bpfhistogram!(
    SYSCALL_SLEEP_LATENCY,
    "syscall/sleep/latency",
    "latency of sleep related syscalls (nanosleep, clock_nanosleep)"
);
bpfhistogram!(
    SYSCALL_SOCKET_LATENCY,
    "syscall/socket/latency",
    "latency of socket related syscalls (accept, connect, bind, setsockopt, ...)"
);
