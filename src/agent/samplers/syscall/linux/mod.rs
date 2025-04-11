mod counts;
mod latency;

pub const MAX_SYSCALL_ID: usize = 1024;

pub fn syscall_lut() -> Vec<u64> {
    (0..MAX_SYSCALL_ID)
        .map(|id| {
            if let Some(syscall_name) = syscall_numbers::native::sys_call_name(id as i64) {
                match syscall_name {
                    // 1: Read related
                    "pread64" | "preadv" | "preadv2" | "read" | "readv" | "recvfrom"
                    | "recvmmsg" | "recvmsg" => 1,

                    // 2: Write related
                    "pwrite64" | "pwritev" | "pwritev2" | "sendmmsg" | "sendmsg" | "sendto"
                    | "write" | "writev" => 2,

                    // 3: Poll/select/epoll
                    "epoll_create" | "epoll_create1" | "epoll_ctl" | "epoll_ctl_old"
                    | "epoll_pwait" | "epoll_pwait2" | "epoll_wait" | "epoll_wait_old" | "poll"
                    | "ppoll" | "ppoll_time64" | "pselect6" | "pselect6_time64" | "select" => 3,

                    // 4: Locking
                    "futex" => 4,

                    // 5: Time
                    "adjtimex" | "clock_adjtime" | "clock_getres" | "clock_gettime"
                    | "clock_settime" | "gettimeofday" | "settimeofday" | "time" => 5,

                    // 6: Sleep
                    "clock_nanosleep" | "nanosleep" => 6,

                    // 7: Socket
                    "accept" | "accept4" | "bind" | "connect" | "getpeername" | "getsockname"
                    | "getsockopt" | "listen" | "setsockopt" | "shutdown" | "socket"
                    | "socketpair" => 7,

                    // 8: Yield
                    "sched_yield" => 8,

                    // 9: Filesystem operations
                    "open" | "openat" | "close" | "creat" | "lseek" | "fsync" | "fdatasync"
                    | "sync" | "syncfs" | "truncate" | "ftruncate" | "rename" | "renameat"
                    | "link" | "symlink" | "unlink" | "readlink" | "stat" | "fstat" | "lstat"
                    | "statx" | "access" | "faccessat" | "chmod" | "fchmod" | "chown"
                    | "fchown" | "lchown" | "utime" | "utimes" | "utimensat" | "mkdir"
                    | "rmdir" | "chdir" | "fchdir" | "getcwd" | "getdents" | "getdents64"
                    | "readdir" => 9,

                    // 10: Memory management
                    "mmap" | "munmap" | "mprotect" | "mremap" | "madvise" | "msync" | "mincore"
                    | "mlock" | "munlock" | "mlockall" | "munlockall" | "brk" | "sbrk" => 10,

                    // 11: Process control
                    "clone" | "fork" | "vfork" | "execve" | "execveat" | "exit" | "exit_group"
                    | "wait4" | "waitid" | "waitpid" | "kill" | "tkill" | "tgkill" | "ptrace"
                    | "prctl" | "setpgid" | "getpgid" | "setpriority" | "getpriority"
                    | "sched_setaffinity" | "sched_getaffinity" | "sched_setscheduler"
                    | "sched_getscheduler" | "sched_setparam" | "sched_getparam" => 11,

                    // 12: Resource query
                    "getrusage" | "getrlimit" | "setrlimit" | "prlimit64" | "times" | "getpid"
                    | "getppid" | "getuid" | "geteuid" | "getgid" | "getegid" | "gettid"
                    | "uname" | "sysinfo" | "getcpu" => 12,

                    // 13: IPC (Inter-Process Communication)
                    "pipe" | "pipe2" | "msgget" | "msgsnd" | "msgrcv" | "msgctl" | "semget"
                    | "semop" | "semctl" | "shmget" | "shmat" | "shmdt" | "shmctl" | "mq_open"
                    | "mq_close" | "mq_unlink" | "mq_send" | "mq_receive" | "mq_getsetattr"
                    | "mq_notify" | "mq_timedreceive" | "mq_timedsend" => 13,

                    // 14: Timers and alarms
                    "alarm" | "getitimer" | "setitimer" | "timer_create" | "timer_delete"
                    | "timer_getoverrun" | "timer_gettime" | "timer_settime" | "timerfd_create"
                    | "timerfd_gettime" | "timerfd_settime" => 14,

                    // 15: Event notification
                    "eventfd" | "eventfd2" | "signalfd" | "signalfd4" | "inotify_init"
                    | "inotify_init1" | "inotify_add_watch" | "inotify_rm_watch"
                    | "fanotify_init" | "fanotify_mark" | "io_setup" | "io_destroy"
                    | "io_submit" | "io_cancel" | "io_getevents" | "io_uring_setup"
                    | "io_uring_enter" | "io_uring_register" => 15,

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
