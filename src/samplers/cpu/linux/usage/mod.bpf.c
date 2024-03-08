// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 16
// #define HISTOGRAM_BUCKETS 7424
#define MAX_CPUS 1024
// #define MAX_SYSCALL_ID 1024
// #define MAX_TRACKED_PIDS 65536 

// counters for cpu usage
// 0 - user
// 1 - nice
// 2 - system
// 3 - idle
// 4 - iowait
// 5 - irq
// 6 - softirq
// 7 - steal
// 8 - guest
// 9 - guest_nice
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");


/*
static void irqtime_account_delta(struct irqtime *irqtime, u64 delta,
				  enum cpu_usage_stat idx)
{
	u64 *cpustat = kcpustat_this_cpu->cpustat;

	u64_stats_update_begin(&irqtime->sync);
	cpustat[idx] += delta;
	irqtime->total += delta;
	irqtime->tick_delta += delta;
	u64_stats_update_end(&irqtime->sync);
}
*/

int account_delta(u64 delta, u32 usage_idx)
{
	u64 *cnt;
	u32 idx;

	idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + usage_idx;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, delta);
	}

	return 0;
}

SEC("kprobe/__cgroup_account_cputime_field")
int BPF_KPROBE(cgroup_account_cputime_field_kprobe, void *task, u32 index, u64 delta)
{
	return account_delta(delta, index);
}

// SEC("kprobe/account_nice_time")
// int BPF_KPROBE(account_nice_time_kprobe, void *task, u64 delta)
// {
// 	return account_delta(delta, 0);
// }

// SEC("kprobe/account_system_index_time")
// int BPF_KPROBE(account_system_index_time_kprobe, void *task, u64 delta, u32 index)
// {
// 	return account_delta(delta, index);
// }

// SEC("kprobe/account_user_time")
// int BPF_KPROBE(account_user_time_kprobe, void *task, u64 delta)
// {
// 	return account_delta(delta, 0);
// }


// SEC("kprobe/irqtime_account_delta")
// int BPF_KPROBE(irqtime_account_delta_kprobe, struct irqtime *irqtime, u64 delta, u32 usage_idx)
// {
// 	return account_delta(delta, usage_idx);
// }

char LICENSE[] SEC("license") = "GPL";