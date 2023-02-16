// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 IOP Systems, Inc.

#include "../../../vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include "../../common/bpf.h"

const volatile pid_t target_pid = 0;
const volatile __u64 min_lat_ns = 0;

enum op {
    F_READ,
    F_WRITE,
    F_OPEN,
    F_FSYNC,
    F_MAX_OP,
};

struct data {
    __u64 ts;
    loff_t start;
    loff_t end;
    struct file *fp;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, __u32);
    __type(value, struct data);
} starts SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} read_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} write_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} open_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} fsync_latency SEC(".maps");

static int probe_entry(struct file *fp, loff_t start, loff_t end)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u32 tid = (__u32)pid_tgid;
    struct data data;

    if (!fp)
        return 0;

    data.ts = bpf_ktime_get_ns();
    data.start = start;
    data.end = end;
    data.fp = fp;
    bpf_map_update_elem(&starts, &tid, &data, BPF_ANY);
    return 0;
}

static int probe_exit(void *ctx, enum op op, ssize_t size)
{
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u32 tid = (__u32)pid_tgid;
    __u64 end_ns, delta_ns;
    u64 *cnt;
    struct data *datap;

    datap = bpf_map_lookup_elem(&starts, &tid);
    if (!datap)
        return 0;

    bpf_map_delete_elem(&starts, &tid);

    end_ns = bpf_ktime_get_ns();
    delta_ns = end_ns - datap->ts;

    u32 idx = value_to_index(delta_ns);

    if (op == F_READ) {
        cnt = bpf_map_lookup_elem(&read_latency, &idx);
    } else if (op == F_WRITE) {
        cnt = bpf_map_lookup_elem(&write_latency, &idx);
    } else if (op == F_OPEN) {
        cnt = bpf_map_lookup_elem(&open_latency, &idx);
    } else if (op == F_FSYNC) {
        cnt = bpf_map_lookup_elem(&fsync_latency, &idx);
    }

    if (!cnt) {
        return 0; /* counter was not in the map */
    }

    __sync_fetch_and_add(cnt, 1);

    return 0;
}

SEC("kprobe/generic_file_read_iter")
int BPF_KPROBE(file_read_entry, struct inode *inode, struct file *file)
{
    return probe_entry(file, 0, 0);
}

SEC("kretprobe/generic_file_read_iter")
int BPF_KRETPROBE(file_read_exit, ssize_t ret)
{
    return probe_exit(ctx, F_READ, ret);
}

SEC("kprobe/generic_file_write_iter")
int BPF_KPROBE(file_write_entry, struct inode *inode, struct file *file)
{
    return probe_entry(file, 0, 0);
}

SEC("kretprobe/generic_file_write_iter")
int BPF_KRETPROBE(file_write_exit, ssize_t ret)
{
    return probe_exit(ctx, F_WRITE, ret);
}

SEC("kprobe/generic_file_open")
int BPF_KPROBE(file_open_entry, struct inode *inode, struct file *file)
{
    return probe_entry(file, 0, 0);
}

SEC("kretprobe/generic_file_open")
int BPF_KRETPROBE(file_open_exit)
{
    return probe_exit(ctx, F_OPEN, 0);
}

SEC("kprobe/generic_file_fsync")
int BPF_KPROBE(file_sync_entry, struct file *file, loff_t start, loff_t end)
{
    return probe_entry(file, start, end);
}

SEC("kretprobe/generic_file_fsync")
int BPF_KRETPROBE(file_sync_exit)
{
    return probe_exit(ctx, F_FSYNC, 0);
}

char LICENSE[] SEC("license") = "GPL";