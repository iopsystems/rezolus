// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2022 Francis Laniel <flaniel@linux.microsoft.com>
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `tcptop.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes TCP send and receive paths to get the number of
// segments and bytes transmitted as well as the size distributions.

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

/* Taken from kernel include/linux/socket.h. */
#define AF_INET		2	/* Internet IP Protocol 	*/
#define AF_INET6	10	/* IP version 6			*/

#define TCP_RX_BYTES 0
#define TCP_TX_BYTES 1
#define TCP_RX_SEGMENTS 2
#define TCP_TX_SEGMENTS 3

// counters
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 8192); // good for up to 1024 cores w/ 8 counters
} counters SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} rx_size SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} tx_size SEC(".maps");

static int probe_ip(bool receiving, struct sock *sk, size_t size)
{
	u16 family;
	u64 *cnt;
	u32 idx;

	family = BPF_CORE_READ(sk, __sk_common.skc_family);

	/* drop */
	if (family != AF_INET && family != AF_INET6) {
		return 0;
	}


	if (receiving) {
		idx = 8 * bpf_get_smp_processor_id() + TCP_RX_BYTES;
		cnt = bpf_map_lookup_elem(&counters, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, (u64) size);
		}

		idx = value_to_index((u64) size);
		cnt = bpf_map_lookup_elem(&rx_size, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		idx = 8 * bpf_get_smp_processor_id() + TCP_RX_SEGMENTS;
		cnt = bpf_map_lookup_elem(&counters, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	} else {
		idx = 8 * bpf_get_smp_processor_id() + TCP_TX_BYTES;
		cnt = bpf_map_lookup_elem(&counters, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, (u64) size);
		}

		idx = value_to_index((u64) size);
		cnt = bpf_map_lookup_elem(&tx_size, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}

		idx = 8 * bpf_get_smp_processor_id() + TCP_TX_SEGMENTS;
		cnt = bpf_map_lookup_elem(&counters, &idx);

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
	if (copied <= 0) {
		return 0;
	}

	return probe_ip(true, sk, copied);
}

char LICENSE[] SEC("license") = "GPL";