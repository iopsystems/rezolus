// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 The Rezolus Authors

// This BPF program probes TCP retransmit path to gather statistics.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// counters
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

SEC("kprobe/tcp_retransmit_skb")
int BPF_KPROBE(tcp_retransmit_skb, struct sock* sk, struct sk_buff* skb, int segs) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();
    array_incr(&counters, idx);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
