use crate::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

heatmap!(SCHEDULER_RUNQUEUE_LATENCY, "scheduler/runqueue/latency", "distribution of task wait times in the runqueue");

counter_with_heatmap!(SYSCALL_TOTAL, SYSCALL_TOTAL_HIST, "syscall/total", "total syscalls");

counter!(SYSCALL_READ, "syscall/read", "read from file descriptor");
counter!(SYSCALL_WRITE, "syscall/write", "write to file descriptor");
counter!(SYSCALL_OPEN, "syscall/open", "open and possibly create a file or device");
counter!(SYSCALL_CLOSE, "syscall/close", "close a file descriptor");
counter!(SYSCALL_RECVFROM, "syscall/recvfrom", "receive a message from a socket");
counter!(SYSCALL_RECVMSG, "syscall/recvmsg", "receive a message from a socket");
counter!(SYSCALL_RECVMMSG, "syscall/recvmmsg", "receive multiple messages from a socket");
counter!(SYSCALL_SENDTO, "syscall/sendto", "send a message on a socket");
counter!(SYSCALL_SENDMSG, "syscall/sendmsg", "send a message on a socket");
counter!(SYSCALL_SHUTDOWN, "syscall/shutdown", "shut down part of a full-duplex connection");
counter!(SYSCALL_BIND, "syscall/bind", "bind a name to a socket");
counter!(SYSCALL_LISTEN, "syscall/listen", "listen for connections on a socket");

