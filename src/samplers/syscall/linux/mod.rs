mod stats;

mod counts;
mod latency;

pub const MAX_SYSCALL_ID: usize = 1024;

pub fn syscall_lut() -> Vec<u64> {
    (0..MAX_SYSCALL_ID)
        .map(|id| {
            if let Some(syscall_name) = syscall_numbers::native::sys_call_name(id as i64) {
                match syscall_name {
                    // read related
                    "pread64" | "preadv" | "preadv2" | "read" | "readv" | "recvfrom"
                    | "recvmmsg" | "recvmsg" => 1,
                    // write related
                    "pwrite64" | "pwritev" | "pwritev2" | "sendmmsg" | "sendmsg" | "sendto"
                    | "write" | "writev" => 2,
                    // poll/select/epoll
                    "epoll_create" | "epoll_create1" | "epoll_ctl" | "epoll_ctl_old"
                    | "epoll_pwait" | "epoll_pwait2" | "epoll_wait" | "epoll_wait_old" | "poll"
                    | "ppoll" | "ppoll_time64" | "pselect6" | "pselect6_time64" | "select" => 3,
                    // locking
                    "futex" => 4,
                    // time
                    "adjtimex" | "clock_adjtime" | "clock_getres" | "clock_gettime"
                    | "clock_settime" | "gettimeofday" | "settimeofday" | "time" => 5,
                    // sleep
                    "clock_nanosleep" | "nanosleep" => 6,
                    // socket creation and management
                    "accept" | "bind" | "connect" | "getpeername" | "getsockname"
                    | "getsockopt" | "listen" | "setsockopt" | "shutdown" | "socket"
                    | "socketpair" => 7,
                    // yield
                    "sched_yield" => 8,
                    _ => {
                        // no group defined for these syscalls
                        0
                    }
                }
            } else {
                0
            }
        })
        .collect()
}
