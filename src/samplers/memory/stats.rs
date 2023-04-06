use crate::*;

gauge!(MEMORY_TOTAL, "memory/total");
gauge!(MEMORY_FREE, "memory/free");
gauge!(MEMORY_AVAILABLE, "memory/available");
gauge!(MEMORY_BUFFERS, "memory/buffers");
gauge!(MEMORY_CACHED, "memory/cached");

counter!(MEMORY_NUMA_HIT, "memory/numa/hit");
counter!(MEMORY_NUMA_MISS, "memory/numa/miss");
counter!(MEMORY_NUMA_FOREIGN, "memory/numa/foreign");
counter!(MEMORY_NUMA_INTERLEAVE, "memory/numa/interleave");
counter!(MEMORY_NUMA_LOCAL, "memory/numa/local");
counter!(MEMORY_NUMA_OTHER, "memory/numa/other");
