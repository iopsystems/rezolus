// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2022 Francis Laniel <flaniel@linux.microsoft.com>
// Copyright (c) 2023 IOP Systems, Inc.

// This BPF program probes TCP send and receive paths to get the number of
// segments and bytes transmitted as well as the size distributions.

#include "../../../../common/bpf/vmlinux.h"
#include "../../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

/* Taken from kernel include/linux/socket.h. */
#define AF_INET		2	/* Internet IP Protocol 	*/
#define AF_INET6	10	/* IP version 6			*/

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 496);
} rx_size SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} rx_bytes SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} rx_segments SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 496);
} tx_size SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} tx_bytes SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} tx_segments SEC(".maps");

static int probe_ip(bool receiving, struct sock *sk, size_t size)
{
	u16 family;
	u64 *cnt;
	u32 idx;

	family = BPF_CORE_READ(sk, __sk_common.skc_family);

	/* drop */
	if (family != AF_INET && family != AF_INET6)
		return 0;


	if (receiving) {
		idx = 0;
		cnt = bpf_map_lookup_elem(&rx_bytes, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, (u64) size);
		}

		idx = value_to_index((u64) size);
		cnt = bpf_map_lookup_elem(&rx_size, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		idx = 0;
		cnt = bpf_map_lookup_elem(&rx_segments, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	} else {
		idx = 0;
		cnt = bpf_map_lookup_elem(&tx_bytes, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, (u64) size);
		}

		idx = value_to_index((u64) size);
		cnt = bpf_map_lookup_elem(&tx_size, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		idx = 0;
		cnt = bpf_map_lookup_elem(&tx_segments, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}

	return 0;
}

SEC("kprobe/tcp_sendmsg")
int BPF_KPROBE(tcp_sendmsg, struct sock *sk, struct msghdr *msg, size_t size)
{
	return probe_ip(false, sk, size);
}

/*
 * tcp_recvmsg() would be obvious to trace, but is less suitable because:
 * - we'd need to trace both entry and return, to have both sock and size
 * - misses tcp_read_sock() traffic
 * we'd much prefer tracepoints once they are available.
 */
SEC("kprobe/tcp_cleanup_rbuf")
int BPF_KPROBE(tcp_cleanup_rbuf, struct sock *sk, int copied)
{
	if (copied <= 0)
		return 0;

	return probe_ip(true, sk, copied);
}

char LICENSE[] SEC("license") = "GPL";