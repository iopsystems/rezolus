// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 The Rezolus Authors

#include <vmlinux.h>
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096

#define TASK_RUNNING 0

// counter positions
#define APERF 0
#define MPERF 1
#define TSC 2

// counters (see constants defined at top)
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_aperf SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_mperf SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_tsc SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} aperf_prev SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} mperf_prev SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} tsc_prev SEC(".maps");

/**
 * perf event arrays
 */

struct {
	__uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
	__type(key, u32);
	__type(value, u32);
} aperf SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
	__type(key, u32);
	__type(value, u32);
} mperf SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
	__type(key, u32);
	__type(value, u32);
} tsc SEC(".maps");

/**
 * commit 2f064a59a1 ("sched: Change task_struct::state") changes
 * the name of task_struct::state to task_struct::__state
 * see:
 *     https://github.com/torvalds/linux/commit/2f064a59a1
 */
struct task_struct___o {
	volatile long int state;
} __attribute__((preserve_access_index));

struct task_struct___x {
	unsigned int __state;
} __attribute__((preserve_access_index));

static __always_inline __s64 get_task_state(void *task)
{
	struct task_struct___x *t = task;

	if (bpf_core_field_exists(t->__state))
		return BPF_CORE_READ(t, __state);
	return BPF_CORE_READ((struct task_struct___o *)task, state);
}

// attach a kprobe cpuacct to update per-cpu counters

SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, void *task, u32 index, u64 delta)
{
	u32 idx;
	u32 processor_id = bpf_get_smp_processor_id();

	u64 a = bpf_perf_event_read(&aperf, BPF_F_CURRENT_CPU);
	u64 m = bpf_perf_event_read(&mperf, BPF_F_CURRENT_CPU);
	u64 t = bpf_perf_event_read(&tsc, BPF_F_CURRENT_CPU);

	idx = processor_id * COUNTER_GROUP_WIDTH + APERF;
	bpf_map_update_elem(&counters, &idx, &a, BPF_ANY);

	idx = processor_id * COUNTER_GROUP_WIDTH + MPERF;
	bpf_map_update_elem(&counters, &idx, &m, BPF_ANY);

	idx = processor_id * COUNTER_GROUP_WIDTH + TSC;
	bpf_map_update_elem(&counters, &idx, &t, BPF_ANY);
}

// attach a tracepoint on sched_switch for per-cgroup accounting

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64 *ctx)
{
	/* TP_PROTO(bool preempt, struct task_struct *prev,
	 *      struct task_struct *next)
	 */
	struct task_struct *prev = (struct task_struct *)ctx[1];
	struct task_struct *next = (struct task_struct *)ctx[2];

	u32 idx;
	u64 *elem, delta_a, delta_m, delta_t;

	u32 processor_id = bpf_get_smp_processor_id();

	u64 a = bpf_perf_event_read(&aperf, BPF_F_CURRENT_CPU);
	u64 m = bpf_perf_event_read(&mperf, BPF_F_CURRENT_CPU);
	u64 t = bpf_perf_event_read(&tsc, BPF_F_CURRENT_CPU);

	idx = processor_id * COUNTER_GROUP_WIDTH + APERF;
	bpf_map_update_elem(&counters, &idx, &a, BPF_ANY);

	idx = processor_id * COUNTER_GROUP_WIDTH + MPERF;
	bpf_map_update_elem(&counters, &idx, &m, BPF_ANY);

	idx = processor_id * COUNTER_GROUP_WIDTH + TSC;
	bpf_map_update_elem(&counters, &idx, &t, BPF_ANY);

	if (bpf_core_field_exists(prev->sched_task_group)) {
		int cgroup_id = prev->sched_task_group->css.id;

		if (cgroup_id && cgroup_id < MAX_CGROUPS) {
			// update cgroup aperf

			elem = bpf_map_lookup_elem(&aperf_prev, &processor_id);

			if (elem) {
				delta_a = a - *elem;

				array_add(&cgroup_aperf, cgroup_id, delta_a);
			}

			// update cgroup mperf

			elem = bpf_map_lookup_elem(&mperf_prev, &processor_id);

			if (elem) {
				delta_m = m - *elem;

				array_add(&cgroup_mperf, cgroup_id, delta_m);
			}

			// update cgroup tsc

			elem = bpf_map_lookup_elem(&tsc_prev, &processor_id);

			if (elem) {
				delta_t = t - *elem;

				array_add(&cgroup_tsc, cgroup_id, delta_t);
			}
		}
	}

	bpf_map_update_elem(&aperf_prev, &processor_id, &a, BPF_ANY);
	bpf_map_update_elem(&mperf_prev, &processor_id, &m, BPF_ANY);
	bpf_map_update_elem(&tsc_prev, &processor_id, &t, BPF_ANY);

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
