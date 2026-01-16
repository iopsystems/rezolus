# Troubleshooting

This document covers known issues and platform-specific limitations that may affect Rezolus metrics collection.

## Missing CPU 0 Hardware Performance Metrics on ARM (Graviton)

### Symptoms

On AWS Graviton instances (and potentially other ARM systems), you may notice that hardware performance counter metrics are missing for CPU 0 while all other CPUs report data correctly. Affected metrics include:

- `cpu_cycles` / `cpu_instructions` (IPC)
- `cpu_dtlb_miss`
- `cpu_branch_instructions` / `cpu_branch_misses`
- CPU frequency metrics

BPF-based metrics like `cpu_usage`, `scheduler_*`, `syscall_*`, etc. are **not affected** and will report data for all CPUs including CPU 0.

### Cause

The Linux NMI (Non-Maskable Interrupt) watchdog uses hardware performance monitoring counters to detect hard lockups. On many ARM systems, the NMI watchdog is pinned to CPU 0 and reserves all available PMU (Performance Monitoring Unit) counters on that CPU.

When Rezolus attempts to create perf events on CPU 0, the kernel rejects them because no PMU counters are available.

You can verify this is the cause by running:

```bash
# Check if NMI watchdog is enabled
cat /proc/sys/kernel/nmi_watchdog

# Try to read cycles on CPU 0 (will fail if NMI watchdog has reserved PMU)
sudo perf stat -e cycles -C 0 sleep 1

# Compare with CPU 1 (should work)
sudo perf stat -e cycles -C 1 sleep 1
```

If CPU 0 shows `<not counted>` while CPU 1 works, the NMI watchdog is the cause.

### Affected Platforms

This has been observed on:
- AWS Graviton4 (c8g, m8g, r8g instance families)
- Potentially other Graviton generations and ARM platforms with NMI watchdog enabled

### Workarounds

#### Option 1: Accept the limitation (Recommended)

For most use cases, missing CPU 0 data for hardware performance counters is acceptable. The NMI watchdog provides important system reliability benefits (detecting hard lockups), and the other CPUs still provide representative performance data.

BPF-based metrics (cpu_usage, scheduler metrics, syscall metrics, etc.) are unaffected and will continue to report data for all CPUs.

#### Option 2: Disable NMI watchdog (Not recommended for production)

If you absolutely need hardware performance counter data for CPU 0, you can disable the NMI watchdog:

```bash
# Temporarily disable (resets on reboot)
echo 0 > /proc/sys/kernel/nmi_watchdog

# Or permanently via kernel parameter
# Add "nmi_watchdog=0" to kernel command line
```

**Warning**: Disabling the NMI watchdog removes the kernel's ability to detect and report hard lockups. This is generally not recommended for production systems.

#### Option 3: Move NMI watchdog to a different CPU

On some systems, you may be able to configure which CPU handles the NMI watchdog, though this is kernel and platform dependent.

### Technical Details

The NMI watchdog works by programming a PMU counter to overflow after a certain number of CPU cycles. When it overflows, it generates an NMI. If the NMI handler doesn't run within a timeout period, the kernel assumes the CPU is locked up.

On ARM systems with limited PMU counters (typically 6 general-purpose counters on Neoverse cores), the NMI watchdog may consume counters in a way that prevents other perf events from being scheduled on CPU 0.

This is a kernel/hardware limitation, not a Rezolus bug. The same limitation affects the `perf` tool and any other software using hardware performance counters.

## L3 Cache Metrics Not Available on ARM (Graviton)

### Symptoms

The `cpu_l3_access` and `cpu_l3_miss` metrics are not available on ARM platforms including AWS Graviton instances.

### Cause

On x86 platforms (AMD and Intel), L3 cache metrics are collected using uncore PMUs that provide cache-level visibility across all cores sharing the L3 cache.

ARM platforms expose L3 cache events (`L3D_CACHE`, `L3D_CACHE_REFILL`) as per-core PMU events rather than per-cache-domain events. These events count each core's requests to the L3, not actual L3 operations. If multiple cores access the same cache line, each core counts it separately - making aggregation across cores misleading. For profiling a specific workload's cache behavior, use `perf stat -p <pid>` with these events instead.

Accurate L3 cache metrics on ARM would require access to the CMN (Coherent Mesh Network) PMU, which provides actual cache slice counters. However, CMN PMU access is often restricted or unavailable on cloud instances.

### Affected Platforms

- All AWS Graviton generations (Graviton2, Graviton3, Graviton4)
- ARM Neoverse-based systems (N1, V1, N2, V2)
- Other ARM platforms using per-core L3 cache events

### Workaround

There is currently no workaround. L3 cache metrics are only available on x86 platforms (AMD Zen and Intel server CPUs) where uncore PMUs provide accurate cache-domain-level visibility.
