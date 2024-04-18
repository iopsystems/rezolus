// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 16
#define MAX_CPUS 1024

#define IDLE_STAT_INDEX 5
#define IOWAIT_STAT_INDEX 6

// cpu usage stat index (https://elixir.bootlin.com/linux/v6.9-rc4/source/include/linux/kernel_stat.h#L20)
// 0 - user
// 1 - nice
// 2 - system
// 3 - softirq
// 4 - irq
// 5 - idle - *NOTE* this will not increment. User-space must calculate it
// 6 - iowait
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

SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, void *task, u32 index, u64 delta)
{
  // ignore both the idle and the iowait counting since both count the idle time
  // https://elixir.bootlin.com/linux/v6.9-rc4/source/kernel/sched/cputime.c#L227
	if (index == IDLE_STAT_INDEX || index == IOWAIT_STAT_INDEX) {
		return 0;
	}

	return account_delta(delta, index);
}

char LICENSE[] SEC("license") = "GPL";