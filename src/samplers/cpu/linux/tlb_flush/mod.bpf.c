// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

// This BPF program tracks tlb_flush events

#include <vmlinux.h>
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// counters for tlb_flush events
// 0 - task_switch
// 1 - remote shootdown
// 2 - local shootdown
// 3 - local mm shootdown
// 4 - remote send ipi
//
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} events SEC(".maps");

SEC("raw_tp/tlb_flush")
int BPF_PROG(tlb_flush, int reason, u64 pages)
{
	u32 offset, idx;

	offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

	idx = reason + offset;

	array_incr(&events, idx);

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
