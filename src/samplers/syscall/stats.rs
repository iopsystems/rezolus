use crate::*;

counter_with_heatmap!(SYSCALL_TOTAL, SYSCALL_TOTAL_HEATMAP, "syscall/total", "tracks total syscalls");
heatmap!(SYSCALL_TOTAL_LATENCY, "syscall/total/latency", "latency of all syscalls");
