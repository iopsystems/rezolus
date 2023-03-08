// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2019 Facebook
// Copyright (c) 2023 IOP Systems, Inc.

// This BPF program probes enqueue and dequeue from the scheduler runqueue
// to calculate the runqueue latency.

#include "../../../../common/bpf/vmlinux.h"
#include "../../../../common/bpf/histogram.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>

#define TASK_RUNNING	0

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

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, u32);
	__type(value, u64);
} enqueued_at SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, u32);
	__type(value, u64);
} running_at SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} runqlat SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} running SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} ivcsw SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} vcsw SEC(".maps");

/* record enqueue timestamp */
static __always_inline
int trace_enqueue(u32 tgid, u32 pid)
{
	u64 ts;

	if (!pid)
		return 0;

	ts = bpf_ktime_get_ns();
	bpf_map_update_elem(&enqueued_at, &pid, &ts, 0);
	return 0;
}

SEC("tp_btf/sched_wakeup")
int handle__sched_wakeup(u64 *ctx)
{
	/* TP_PROTO(struct task_struct *p) */
	struct task_struct *p = (void *)ctx[0];

	return trace_enqueue(p->tgid, p->pid);
}

SEC("tp_btf/sched_wakeup_new")
int handle__sched_wakeup_new(u64 *ctx)
{
	/* TP_PROTO(struct task_struct *p) */
	struct task_struct *p = (void *)ctx[0];

	return trace_enqueue(p->tgid, p->pid);
}

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64 *ctx)
{
	/* TP_PROTO(bool preempt, struct task_struct *prev,
	 *	    struct task_struct *next)
	 */
	struct task_struct *prev = (struct task_struct *)ctx[1];
	struct task_struct *next = (struct task_struct *)ctx[2];

	u32 pid;
	u64 *tsp, delta_ns, *cnt;

	u64 ts = bpf_ktime_get_ns();


	// if prev was TASK_RUNNING, calculate how long prev was running, increment hist
	// if prev was TASK_RUNNING, increment ivcsw counter
	// if prev was TASK_RUNNING, trace enqueue of prev

	// prev task is moving from running
	// - update prev->pid enqueued_at with now
	// - calculate how long prev task was running, update hist
	if (get_task_state(prev) == TASK_RUNNING) {
		// count involuntary context switch
		u32 idx = 0;
		cnt = bpf_map_lookup_elem(&ivcsw, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		pid = prev->pid;

		// mark when it was enqueued
		bpf_map_update_elem(&enqueued_at, &pid, &ts, 0);

		// calculate how long it was running, increment running histogram
		tsp = bpf_map_lookup_elem(&running_at, &pid);
		if (tsp) {
			delta_ns = ts - *tsp;
			u32 idx = value_to_index(delta_ns);
			cnt = bpf_map_lookup_elem(&running, &idx);
			if (cnt) {
				__sync_fetch_and_add(cnt, 1);
			}

			bpf_map_delete_elem(&running_at, &pid);
		}
	} else {
		// count voluntary context switch
		u32 idx = 0;
		cnt = bpf_map_lookup_elem(&vcsw, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}
	
	// next task has moved into running
	// - update next->pid running_at with now
	// - calculate how long next task was enqueued, update hist
	pid = next->pid;

	// update running_at
	bpf_map_update_elem(&running_at, &pid, &ts, 0);

	// calculate how long it was enqueued, increment running histogram
	tsp = bpf_map_lookup_elem(&enqueued_at, &pid);
	if (tsp) {
		delta_ns = ts - *tsp;
		u32 idx = value_to_index(delta_ns);
		cnt = bpf_map_lookup_elem(&runqlat, &idx);
		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		bpf_map_delete_elem(&enqueued_at, &pid);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";