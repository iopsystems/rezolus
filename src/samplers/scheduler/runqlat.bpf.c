// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2019 Facebook
// Copyright (c) 2023 IOP Systems
#include "../../../vmlinux.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
// #include "runqlat.h"
#include "../../common/bpf.h"

#define TASK_RUNNING	0

// // Dummy instance to get skeleton to generate definition for `struct event`
// struct event _event = {0};

// Kernel 5.14 changed the state field to __state
struct task_struct___pre_5_14 {
	long int state;
};

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, u32);
	__type(value, u64);
} start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 731);
} hist SEC(".maps");

// // histogram indexing
// static __always_inline u32 value_to_index2(u64 value) {
//     unsigned int index = 460;
//     if (value < 100) {
//         // 0-99 => [0..100)
//         // 0 => 0
//         // 99 => 99
//         index = value;
//     } else if (value < 1000) {
//         // 100-999 => [100..190)
//         // 100 => 100
//         // 999 => 189
//         index = 90 + value / 10;
//     } else if (value < 10000) {
//         // 1_000-9_999 => [190..280)
//         // 1000 => 190
//         // 9999 => 279
//         index = 180 + value / 100;
//     } else if (value < 100000) {
//         // 10_000-99_999 => [280..370)
//         // 10000 => 280
//         // 99999 => 369
//         index = 270 + value / 1000;
//     } else if (value < 1000000) {
//         // 100_000-999_999 => [370..460)
//         // 100_000 => 370
//         // 999_999 => 459
//         index = 360 + value / 10000;
//     } else if (value < 10000000) {
//         // 1_000_000-9_999_999 => [460..550)
//         // 1_000_000 => 460
//         // 9_999_999 => 449
//         index = 450 + value / 100000;
//     } else if (value < 100000000) {
//         // 10_000_000-99_999_999 => [550..640)
//         // 10_000_000 => 550
//         // 99_999_999 => 639
//         index = 540 + value / 1000000;
//     } else if (value < 100000000) {
//         // 100_000_000-999_999_999 => [640..730)
//         // 100_000_000 => 640
//         // 999_999_999 => 729
//         index = 630 + value / 10000000;
//     } else {
//         index = 730;
//     }
//     return index;
// }

/* record enqueue timestamp */
static __always_inline
int trace_enqueue(u32 tgid, u32 pid)
{
	u64 ts;

	if (!pid)
		return 0;

	ts = bpf_ktime_get_ns();
	bpf_map_update_elem(&start, &pid, &ts, 0);
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

static inline long get_task_state(struct task_struct *t)
{
	if (bpf_core_field_exists(t->__state))
		return t->__state;

	return ((struct task_struct___pre_5_14*)t)->state;
}

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64 *ctx)
{
	/* TP_PROTO(bool preempt, struct task_struct *prev,
	 *	    struct task_struct *next)
	 */
	struct task_struct *prev = (struct task_struct *)ctx[1];
	struct task_struct *next = (struct task_struct *)ctx[2];
	// struct event event = {};
	u64 *tsp, delta_ns, *cnt;
	long state = get_task_state(prev);
	u32 pid;

	/* ivcsw: treat like an enqueue event and store timestamp */
	if (state == TASK_RUNNING)
		trace_enqueue(prev->tgid, prev->pid);

	pid = next->pid;

	/* fetch timestamp and calculate delta */
	tsp = bpf_map_lookup_elem(&start, &pid);
	if (!tsp)
		return 0;   /* missed enqueue */

	delta_ns = bpf_ktime_get_ns() - *tsp;

	// event.pid = pid;
	// event.delta_ns = delta_ns;

	u32 idx = value_to_index2(delta_ns);
	cnt = bpf_map_lookup_elem(&hist, &idx);

	if (!cnt) {
		return 0; /* counter was not in the map */
	}

	__sync_fetch_and_add(cnt, 1);

	bpf_map_delete_elem(&start, &pid);
	return 0;
}

char LICENSE[] SEC("license") = "GPL";