use crate::*;

gauge_with_heatmap!(
    GPU_CLOCK_GRAPHICS,
    GPU_CLOCK_GRAPHICS_HEATMAP,
    "gpu/clock/graphics"
);
gauge_with_heatmap!(
    GPU_CLOCK_COMPUTE,
    GPU_CLOCK_COMPUTE_HEATMAP,
    "gpu/clock/compute"
);
gauge_with_heatmap!(
    GPU_CLOCK_MEMORY,
    GPU_CLOCK_MEMORY_HEATMAP,
    "gpu/clock/memory"
);
gauge_with_heatmap!(GPU_CLOCK_VIDEO, GPU_CLOCK_VIDEO_HEATMAP, "gpu/clock/video");
gauge_with_heatmap!(
    GPU_MAX_CLOCK_GRAPHICS,
    GPU_MAX_CLOCK_GRAPHICS_HEATMAP,
    "gpu/clock/graphics/max"
);
gauge_with_heatmap!(
    GPU_MAX_CLOCK_COMPUTE,
    GPU_MAX_CLOCK_COMPUTE_HEATMAP,
    "gpu/clock/compute/max"
);
gauge_with_heatmap!(
    GPU_MAX_CLOCK_MEMORY,
    GPU_MAX_CLOCK_MEMORY_HEATMAP,
    "gpu/clock/memory/max"
);
gauge_with_heatmap!(
    GPU_MAX_CLOCK_VIDEO,
    GPU_MAX_CLOCK_VIDEO_HEATMAP,
    "gpu/clock/video/max"
);

gauge_with_heatmap!(GPU_MEMORY_FREE, GPU_MEMORY_FREE_HEATMAP, "gpu/memory/free");
gauge_with_heatmap!(
    GPU_MEMORY_TOTAL,
    GPU_MEMORY_TOTAL_HEATMAP,
    "gpu/memory/total"
);
gauge_with_heatmap!(GPU_MEMORY_USED, GPU_MEMORY_USED_HEATMAP, "gpu/memory/used");

gauge_with_heatmap!(
    GPU_PCIE_BANDWIDTH,
    GPU_PCIE_BANDWIDTH_HEATMAP,
    "gpu/pcie/bandwidth"
);
gauge_with_heatmap!(
    GPU_PCIE_THROUGHPUT_RX,
    GPU_PCIE_THROUGHPUT_RX_HEATMAP,
    "gpu/pcie/throughput/receive"
);
gauge_with_heatmap!(
    GPU_PCIE_THROUGHPUT_TX,
    GPU_PCIE_THROUGHPUT_TX_HEATMAP,
    "gpu/pcie/throughput/transmit"
);

gauge_with_heatmap!(GPU_POWER_USAGE, GPU_POWER_USAGE_HEATMAP, "gpu/power/usage");

gauge_with_heatmap!(GPU_TEMPERATURE, GPU_TEMPERATURE_HEATMAP, "gpu/temperature");
