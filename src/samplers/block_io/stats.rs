use crate::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

heatmap!(
    BLOCKIO_LATENCY,
    "blockio/latency",
    "distribution of block IO latencies"
);
heatmap!(
    BLOCKIO_SIZE,
    "blockio/size",
    "distribution of block IO sizes"
);

// counter!(BLOCKIO_CACHE_TOTAL, "blockio/cache/total", "total count of pagecache accesses, including mark_buffer_dirty events");
// counter!(BLOCKIO_CACHE_MISS, "blockio/cache/miss", "total count of pagecache misses, including forced miss due to dirtied pages");
// counter!(BLOCKIO_CACHE_MBD, "blockio/cache/mbd", "total number of mark_buffer_dirty events");
// counter!(BLOCKIO_CACHE_DIRTIED, "blcokio/cache/dirtied", "number of forced misses due to dirtied pages");
