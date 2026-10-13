#include <bpf/bpf_helpers.h>
#include "histogram.h"

// Per-pid arrays are indexed through pid_slot(), not by the pid, and are
// declared BPF_F_MMAPABLE, which starts their elements on a page boundary.
// A BPF array stores each element in at least 8 bytes, so indexed by pid,
// eight entries share a 64-byte cache line: CPUs running threads with
// neighbouring pids would write the same line. pid_slot() multiplies the
// pid's offset within its block of 512 (one 4 KiB page of u64s) by an odd
// constant mod 512, which permutes the block; the line within the page is
// bits 3-8 of the result. With 277, 48 pids at a constant stride of 1, 2,
// 3, 4, 6, 8, 12, 14, 16, 24, 32 or 64 fall in 48 different lines. Strides 5
// and 7 give 44 and 47 lines, and the other strides from 9 to 17 give 38-42,
// with at most 2 threads in a line. Indexed by pid, 48 consecutive pids fall
// in 6 or 7 lines. The pages a set of pids touches are the same as indexing
// by pid, and the array size and bounds check are unchanged.
// Userspace that reads or labels a per-pid array by index uses the same
// mapping (`pid_slot` in src/agent/bpf/mod.rs).
static __always_inline u32 pid_slot(u32 pid) {
    return (pid & ~511U) | ((pid * 277) & 511);
}

static __always_inline void array_add(void* array, u32 idx, u64 value) {
    u64* elem;

    elem = bpf_map_lookup_elem(array, &idx);

    if (elem) {
        __atomic_fetch_add(elem, value, __ATOMIC_RELAXED);
    }
}

static __always_inline void array_incr(void* array, u32 idx) {
    array_add(array, idx, 1);
}

// A histogram map holds one slice of buckets per CPU, so CPUs recording into
// the same bucket do not write the same cache line; userspace sums the slices.
// The map is declared with HISTOGRAM_BUCKETS * HISTOGRAM_SLICES entries (the
// agent's MAX_CPUS) and the builder sets it to the possible CPUs before load.
// A slice is (65 - grouping_power) << grouping_power buckets: 496 at power 3,
// 62 cache lines, so each slice starts on a line. That holds at any
// grouping power of 3 or more (the width is a multiple of 8 buckets) and
// not below 3.
#define HISTOGRAM_SLICES 1024
#define HISTOGRAM_SLICE_BUCKETS(grouping_power) ((65 - (grouping_power)) << (grouping_power))
_Static_assert(HISTOGRAM_SLICE_BUCKETS(3) == HISTOGRAM_BUCKETS_POW_3,
               "a slice must be as wide as the bucket count the builder checks");

// The index of `value`'s bucket in this CPU's slice.
static __always_inline u32 histogram_index(u64 value, u8 grouping_power) {
    return bpf_get_smp_processor_id() * HISTOGRAM_SLICE_BUCKETS(grouping_power) +
           value_to_index(value, grouping_power);
}

static __always_inline void histogram_incr(void* array, u8 grouping_power, u64 value) {
    array_add(array, histogram_index(value, grouping_power), 1);
}

static __always_inline void array_set_if_larger(void* array, u32 idx, u64 value) {
    u64* elem;

    elem = bpf_map_lookup_elem(array, &idx);

    if (elem && value > *elem) {
        *elem = value;
    }
}