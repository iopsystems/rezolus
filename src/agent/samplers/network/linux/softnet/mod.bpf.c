// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define COUNTER_GROUP_WIDTH 8  // Multiple of 8 for cacheline alignment
#define MAX_CPUS 1024

// counter positions
#define TIME_SQUEEZED 0
#define BUDGET_EXHAUSTED 1
#define PACKETS_PROCESSED 2
#define POLL_COUNT 3

// counters array for per-cpu metrics
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// context tracking for each cpu's net_rx_action execution
struct softnet_ctx {
    u64 start_time;         // when processing started
    u64 packets_processed;  // count of packets processed
    u8 found_work;          // whether work was found (distinguishes no-work from time-limit)
    u8 has_more_work;       // whether there's still more work to do
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, struct softnet_ctx);
    __uint(max_entries, MAX_CPUS);
} cpu_context SEC(".maps");

// track net_rx_action entry point to capture start of processing
SEC("kprobe/net_rx_action")
int BPF_KPROBE(net_rx_action_enter, struct softnet_data *sd)
{
    u32 cpu = bpf_get_smp_processor_id();
    struct softnet_ctx cpu_ctx = {};
    
    cpu_ctx.start_time = bpf_ktime_get_ns();
    cpu_ctx.found_work = 0;
    cpu_ctx.has_more_work = 0;
    cpu_ctx.packets_processed = 0;
    
    bpf_map_update_elem(&cpu_context, &cpu, &cpu_ctx, BPF_ANY);
    return 0;
}

// track when a poll function runs, which means we found work to do
SEC("kprobe/__napi_poll")
int BPF_KPROBE(napi_poll_enter_fn, struct napi_struct *napi, int weight)
{
    u32 cpu = bpf_get_smp_processor_id();
    struct softnet_ctx *cpu_ctx;
    
    cpu_ctx = bpf_map_lookup_elem(&cpu_context, &cpu);
    if (cpu_ctx) {
        cpu_ctx->found_work = 1;
        
        // increment poll count
        u64 offset = COUNTER_GROUP_WIDTH * cpu;
        u32 idx = offset + POLL_COUNT;
        array_incr(&counters, idx);
    }
    
    return 0;
}

// track when a poll function completes with more work to do
SEC("kretprobe/__napi_poll")
int BPF_KRETPROBE(napi_poll_exit_fn, int ret)
{
    u32 cpu = bpf_get_smp_processor_id();
    struct softnet_ctx *cpu_ctx;
    
    cpu_ctx = bpf_map_lookup_elem(&cpu_context, &cpu);
    if (cpu_ctx && ret > 0) {
        // A return value > 0 indicates there's still more work to do
        // This is important for detecting time squeezes vs normal completion
        cpu_ctx->has_more_work = 1;
    }
    
    return 0;
}

// track packet processing events to accurately count packets handled
SEC("kprobe/napi_gro_receive")
int BPF_KPROBE(napi_gro_receive_kprobe, struct napi_struct *napi, struct sk_buff *skb)
{
    u32 cpu = bpf_get_smp_processor_id();
    struct softnet_ctx *cpu_ctx;
    
    cpu_ctx = bpf_map_lookup_elem(&cpu_context, &cpu);
    if (cpu_ctx) {
        // increment packet count
        cpu_ctx->packets_processed++;
        
        // update packets_processed counter
        u64 offset = COUNTER_GROUP_WIDTH * cpu;
        u32 idx = offset + PACKETS_PROCESSED;
        array_incr(&counters, idx);
    }
    
    return 0;
}

// track when net_rx_action exits and determine the reason
SEC("kretprobe/net_rx_action")
int BPF_KRETPROBE(net_rx_action_exit, int ret)
{
    u32 cpu = bpf_get_smp_processor_id();
    struct softnet_ctx *cpu_ctx;
    u64 offset = COUNTER_GROUP_WIDTH * cpu;
    u32 idx;
    u64 duration;
    
    cpu_ctx = bpf_map_lookup_elem(&cpu_context, &cpu);
    if (!cpu_ctx) {
        return 0;
    }
    
    // Calculate processing duration
    duration = bpf_ktime_get_ns() - cpu_ctx->start_time;
    
    // A time squeeze occurs when:
    // 1. We found work (poll was called)
    // 2. There is still more work to do (napi poll returned > 0)
    // 3. We exited with ret==0 (not because we consumed the budget)
    // 4. Duration is close to the max time allowed (around 1ms)
    if (cpu_ctx->found_work && cpu_ctx->has_more_work && ret == 0 && duration > 900000) {
        idx = offset + TIME_SQUEEZED;
        array_incr(&counters, idx);
    } 
    // A budget exhausted case is when we return a positive number
    // indicating how much work we've done
    else if (ret > 0) {
        idx = offset + BUDGET_EXHAUSTED;
        array_incr(&counters, idx);
    }
    
    return 0;
}

char LICENSE[] SEC("license") = "GPL";