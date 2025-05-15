#include <bpf/bpf_helpers.h>
#include "histogram.h"

static __always_inline void array_add(void *array, u32 idx, u64 value) {
    u64 *elem;

    elem = bpf_map_lookup_elem(array, &idx);

    if (elem) {
        __atomic_fetch_add(elem, value, __ATOMIC_RELAXED);
    }
}

static __always_inline void array_incr(void *array, u32 idx) {
    array_add(array, idx, 1);
}

static __always_inline void histogram_incr(void *array, u8 grouping_power, u64 value) {
    u32 idx = value_to_index(value, grouping_power);
    array_add(array, idx, 1);
}

static __always_inline void array_set_if_larger(void *array, u32 idx, u64 value) {
  u64 *elem;

  elem = bpf_map_lookup_elem(array, &idx);

  if (elem && value > *elem) {
    *elem = value;
  }
}