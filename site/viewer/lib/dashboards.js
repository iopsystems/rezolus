// Dashboard definitions — ported from rezolus/src/viewer/dashboard/*.rs
// Each section defines groups of plots with PromQL queries.

const line = (title, id, unit, extra = {}) => ({
    title, id, style: 'line',
    format: { unit_system: unit, precision: 2, ...extra },
});

const heatmap = (title, id, unit, extra = {}) => ({
    title, id, style: 'heatmap',
    format: { unit_system: unit, precision: 2, ...extra },
});

const scatter = (title, id, unit, extra = {}) => ({
    title, id, style: 'scatter',
    format: { unit_system: unit, precision: 2, ...extra },
});

const multi = (title, id, unit, extra = {}) => ({
    title, id, style: 'multi',
    format: { unit_system: unit, precision: 2, ...extra },
});

const plot = (opts, promql_query) => ({
    opts,
    data: [],
    promql_query,
});

const SECTIONS = [
    { name: 'Overview', route: '/overview' },
    { name: 'Query Explorer', route: '/query' },
    { name: 'CPU', route: '/cpu' },
    { name: 'GPU', route: '/gpu' },
    { name: 'Memory', route: '/memory' },
    { name: 'Network', route: '/network' },
    { name: 'Scheduler', route: '/scheduler' },
    { name: 'Syscall', route: '/syscall' },
    { name: 'Softirq', route: '/softirq' },
    { name: 'BlockIO', route: '/blockio' },
    { name: 'cgroups', route: '/cgroups' },
    { name: 'Rezolus', route: '/rezolus' },
];

const SYSCALL_OPS = [
    'Read', 'Write', 'Poll', 'Socket', 'Lock', 'Time', 'Sleep',
    'Yield', 'Filesystem', 'Memory', 'Process', 'Query', 'IPC',
    'Timer', 'Event', 'Other',
];

const SOFTIRQ_KINDS = [
    ['Hardware Interrupts', 'hi'],
    ['IRQ Poll', 'irq_poll'],
    ['Network Transmit', 'net_tx'],
    ['Network Receive', 'net_rx'],
    ['RCU', 'rcu'],
    ['Sched', 'sched'],
    ['Tasklet', 'tasklet'],
    ['Timer', 'timer'],
    ['HR Timer', 'hrtimer'],
    ['Block', 'block'],
];

function generateOverview(info) {
    const groups = [];

    groups.push({
        name: 'CPU', id: 'cpu', plots: [
            plot(line('Busy %', 'cpu-busy', 'percentage'), 'sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('Busy %', 'cpu-busy-heatmap', 'percentage'), 'sum by (id) (irate(cpu_usage[5m])) / 1000000000'),
        ],
    });

    groups.push({
        name: 'Network', id: 'network', plots: [
            plot(line('Transmit Bandwidth', 'network-transmit-bandwidth', 'bitrate'), 'sum(irate(network_bytes{direction="transmit"}[5m])) * 8'),
            plot(line('Receive Bandwidth', 'network-receive-bandwidth', 'bitrate'), 'sum(irate(network_bytes{direction="receive"}[5m])) * 8'),
            plot(line('Transmit Packets', 'network-transmit-packets', 'rate'), 'sum(irate(network_packets{direction="transmit"}[5m]))'),
            plot(line('Receive Packets', 'network-receive-packets', 'rate'), 'sum(irate(network_packets{direction="receive"}[5m]))'),
            plot(scatter('TCP Packet Latency', 'tcp-packet-latency', 'time', { log_scale: true, y_axis_label: 'Latency' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], tcp_packet_latency)'),
        ],
    });

    groups.push({
        name: 'Scheduler', id: 'scheduler', plots: [
            plot(scatter('Runqueue Latency', 'scheduler-runqueue-latency', 'time', { log_scale: true, y_axis_label: 'Latency' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_runqueue_latency)'),
        ],
    });

    groups.push({
        name: 'Syscall', id: 'syscall', plots: [
            plot(line('Total', 'syscall-total', 'rate'), 'sum(irate(syscall[5m]))'),
            plot(scatter('Total', 'syscall-total-latency', 'time', { log_scale: true }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency)'),
        ],
    });

    groups.push({
        name: 'Softirq', id: 'softirq', plots: [
            plot(line('Rate', 'softirq-total-rate', 'rate'), 'sum(irate(softirq[5m]))'),
            plot(heatmap('Rate', 'softirq-total-rate-heatmap', 'rate'), 'sum by (id) (irate(softirq[5m]))'),
            plot(line('CPU %', 'softirq-total-time', 'percentage'), 'sum(irate(softirq_time[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('CPU %', 'softirq-total-time-heatmap', 'percentage'), 'sum by (id) (irate(softirq_time[5m])) / 1000000000'),
        ],
    });

    groups.push({
        name: 'BlockIO', id: 'blockio', plots: [
            plot(line('Read Throughput', 'blockio-throughput-read', 'datarate'), 'sum(irate(blockio_bytes{op="read"}[5m]))'),
            plot(line('Write Throughput', 'blockio-throughput-write', 'datarate'), 'sum(irate(blockio_bytes{op="write"}[5m]))'),
            plot(line('Read IOPS', 'blockio-iops-read', 'count'), 'sum(irate(blockio_operations{op="read"}[5m]))'),
            plot(line('Write IOPS', 'blockio-iops-write', 'count'), 'sum(irate(blockio_operations{op="write"}[5m]))'),
        ],
    });

    return groups;
}

function generateCPU() {
    const groups = [];

    groups.push({
        name: 'Utilization', id: 'utilization', plots: [
            plot(line('Busy %', 'busy-pct', 'percentage'), 'sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('Busy % (Per-CPU)', 'busy-pct-per-cpu', 'percentage'), 'sum by (id) (irate(cpu_usage[5m])) / 1000000000'),
            plot(line('User %', 'user-pct', 'percentage'), 'sum(irate(cpu_usage{state="user"}[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('User % (Per-CPU)', 'user-pct-per-cpu', 'percentage'), 'sum by (id) (irate(cpu_usage{state="user"}[5m])) / 1000000000'),
            plot(line('System %', 'system-pct', 'percentage'), 'sum(irate(cpu_usage{state="system"}[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('System % (Per-CPU)', 'system-pct-per-cpu', 'percentage'), 'sum by (id) (irate(cpu_usage{state="system"}[5m])) / 1000000000'),
        ],
    });

    groups.push({
        name: 'Performance', id: 'performance', plots: [
            plot(line('Instructions per Cycle (IPC)', 'ipc', 'count'), 'sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))'),
            plot(heatmap('IPC (Per-CPU)', 'ipc-per-cpu', 'count'), 'sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m]))'),
            plot(line('Instructions per Nanosecond (IPNS)', 'ipns', 'count'), 'sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m])) * sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / 1000000000 / cpu_cores'),
            plot(heatmap('IPNS (Per-CPU)', 'ipns-per-cpu', 'count'), 'sum by (id) (irate(cpu_instructions[5m])) / sum by (id) (irate(cpu_cycles[5m])) * sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m])) / 1000000000'),
            plot(line('L3 Hit %', 'l3-hit', 'percentage'), '1 - sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))'),
            plot(heatmap('L3 Hit % (Per-CPU)', 'l3-hit-per-cpu', 'percentage'), '1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))'),
            plot(line('Frequency', 'frequency', 'frequency'), 'sum(irate(cpu_tsc[5m])) * sum(irate(cpu_aperf[5m])) / sum(irate(cpu_mperf[5m])) / cpu_cores'),
            plot(heatmap('Frequency (Per-CPU)', 'frequency-per-cpu', 'frequency'), 'sum by (id) (irate(cpu_tsc[5m])) * sum by (id) (irate(cpu_aperf[5m])) / sum by (id) (irate(cpu_mperf[5m]))'),
        ],
    });

    groups.push({
        name: 'Branch Prediction', id: 'branch-prediction', plots: [
            plot(line('Misprediction Rate %', 'branch-miss-rate', 'percentage'), 'sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))'),
            plot(heatmap('Misprediction Rate % (Per-CPU)', 'branch-miss-rate-per-cpu', 'percentage'), 'sum by (id) (irate(cpu_branch_misses[5m])) / sum by (id) (irate(cpu_branch_instructions[5m]))'),
            plot(line('Instructions', 'branch-instructions', 'rate'), 'sum(irate(cpu_branch_instructions[5m]))'),
            plot(heatmap('Instructions (Per-CPU)', 'branch-instructions-per-cpu', 'rate'), 'sum by (id) (irate(cpu_branch_instructions[5m]))'),
            plot(line('Misses', 'branch-misses', 'rate'), 'sum(irate(cpu_branch_misses[5m]))'),
            plot(heatmap('Misses (Per-CPU)', 'branch-misses-per-cpu', 'rate'), 'sum by (id) (irate(cpu_branch_misses[5m]))'),
        ],
    });

    groups.push({
        name: 'DTLB', id: 'dtlb', plots: [
            plot(line('Misses', 'dtlb-misses', 'rate'), 'sum(irate(cpu_dtlb_miss[5m]))'),
            plot(heatmap('Misses (Per-CPU)', 'dtlb-misses-per-cpu', 'rate'), 'sum by (id) (irate(cpu_dtlb_miss[5m]))'),
            plot(line('MPKI', 'dtlb-mpki', 'count'), 'sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m])) * 1000'),
            plot(heatmap('MPKI (Per-CPU)', 'dtlb-mpki-per-cpu', 'count'), 'sum by (id) (irate(cpu_dtlb_miss[5m])) / sum by (id) (irate(cpu_instructions[5m])) * 1000'),
        ],
    });

    groups.push({
        name: 'Migrations', id: 'migrations', plots: [
            plot(line('To', 'cpu-migrations-to', 'rate'), 'sum(irate(cpu_migrations{direction="to"}[5m]))'),
            plot(heatmap('To (Per-CPU)', 'cpu-migrations-to-per-cpu', 'rate'), 'sum by (id) (irate(cpu_migrations{direction="to"}[5m]))'),
            plot(line('From', 'cpu-migrations-from', 'rate'), 'sum(irate(cpu_migrations{direction="from"}[5m]))'),
            plot(heatmap('From (Per-CPU)', 'cpu-migrations-from-per-cpu', 'rate'), 'sum by (id) (irate(cpu_migrations{direction="from"}[5m]))'),
        ],
    });

    groups.push({
        name: 'TLB Flush', id: 'tlb-flush', plots: [
            plot(line('Total', 'tlb-total', 'rate'), 'sum(irate(cpu_tlb_flush[5m]))'),
            plot(heatmap('Total (Per-CPU)', 'tlb-total-per-cpu', 'rate'), 'sum by (id) (irate(cpu_tlb_flush[5m]))'),
            plot(line('Local MM Shootdown', 'tlb-local-mm-shootdown', 'rate'), 'sum(irate(cpu_tlb_flush{reason="local_mm_shootdown"}[5m]))'),
            plot(heatmap('Local MM Shootdown (Per-CPU)', 'tlb-local-mm-shootdown-per-cpu', 'rate'), 'sum by (id) (irate(cpu_tlb_flush{reason="local_mm_shootdown"}[5m]))'),
            plot(line('Remote Send IPI', 'tlb-remote-send-ipi', 'rate'), 'sum(irate(cpu_tlb_flush{reason="remote_send_ipi"}[5m]))'),
            plot(heatmap('Remote Send IPI (Per-CPU)', 'tlb-remote-send-ipi-per-cpu', 'rate'), 'sum by (id) (irate(cpu_tlb_flush{reason="remote_send_ipi"}[5m]))'),
            plot(line('Remote Shootdown', 'tlb-remote-shootdown', 'rate'), 'sum(irate(cpu_tlb_flush{reason="remote_shootdown"}[5m]))'),
            plot(heatmap('Remote Shootdown (Per-CPU)', 'tlb-remote-shootdown-per-cpu', 'rate'), 'sum by (id) (irate(cpu_tlb_flush{reason="remote_shootdown"}[5m]))'),
            plot(line('Task Switch', 'tlb-task-switch', 'rate'), 'sum(irate(cpu_tlb_flush{reason="task_switch"}[5m]))'),
            plot(heatmap('Task Switch (Per-CPU)', 'tlb-task-switch-per-cpu', 'rate'), 'sum by (id) (irate(cpu_tlb_flush{reason="task_switch"}[5m]))'),
        ],
    });

    return groups;
}

function generateMemory() {
    const groups = [];

    groups.push({
        name: 'Usage', id: 'usage', plots: [
            plot(line('Total', 'total', 'bytes'), 'memory_total'),
            plot(line('Available', 'available', 'bytes'), 'memory_available'),
            plot(line('Free', 'free', 'bytes'), 'memory_free'),
            plot(line('Buffers', 'buffers', 'bytes'), 'memory_buffers'),
            plot(line('Cached', 'cached', 'bytes'), 'memory_cached'),
            plot(line('Used', 'used', 'bytes'), 'memory_total - memory_available'),
            plot(line('Utilization %', 'utilization-pct', 'percentage'), '(memory_total - memory_available) / memory_total'),
        ],
    });

    groups.push({
        name: 'NUMA', id: 'numa', plots: [
            plot(line('Local Rate', 'numa-local-rate', 'rate'), 'rate(memory_numa_local[5m])'),
            plot(line('Remote Rate', 'numa-remote-rate', 'rate'), 'rate(memory_numa_foreign[5m])'),
        ],
    });

    return groups;
}

function generateNetwork() {
    const groups = [];

    groups.push({
        name: 'Traffic', id: 'traffic', plots: [
            plot(line('Bandwidth Transmit', 'bandwidth-tx', 'bitrate'), 'sum(irate(network_bytes{direction="transmit"}[5m])) * 8'),
            plot(line('Bandwidth Receive', 'bandwidth-rx', 'bitrate'), 'sum(irate(network_bytes{direction="receive"}[5m])) * 8'),
            plot(line('Packets Transmit', 'packets-tx', 'rate'), 'sum(irate(network_packets{direction="transmit"}[5m]))'),
            plot(line('Packets Receive', 'packets-rx', 'rate'), 'sum(irate(network_packets{direction="receive"}[5m]))'),
        ],
    });

    groups.push({
        name: 'Errors', id: 'errors', plots: [
            plot(line('Packet Drops', 'packet-drops', 'rate'), 'sum(irate(network_drop[5m]))'),
            plot(line('TCP Retransmits', 'tcp-retransmits', 'rate'), 'sum(irate(tcp_retransmit[5m]))'),
        ],
    });

    groups.push({
        name: 'TCP', id: 'tcp', plots: [
            plot(scatter('TCP Packet Latency', 'tcp-packet-latency', 'time', { log_scale: true, y_axis_label: 'Latency' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999], tcp_packet_latency)'),
        ],
    });

    return groups;
}

function generateScheduler() {
    return [{
        name: 'Scheduler', id: 'scheduler', plots: [
            plot(scatter('Runqueue Latency', 'scheduler-runqueue-latency', 'time', { log_scale: true, y_axis_label: 'Latency' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_runqueue_latency)'),
            plot(scatter('Off CPU Time', 'off-cpu-time', 'time', { log_scale: true, y_axis_label: 'Time' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_offcpu)'),
            plot(scatter('Running Time', 'running-time', 'time', { log_scale: true, y_axis_label: 'Time' }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_running)'),
            plot(line('Context Switch', 'cswitch', 'rate'), 'sum(irate(scheduler_context_switch[5m]))'),
        ],
    }];
}

function generateSyscall() {
    const groups = [];

    const totalPlots = [
        plot(line('Total', 'syscall-total', 'rate'), 'sum(irate(syscall[5m]))'),
        plot(scatter('Total', 'syscall-total-latency', 'time', { log_scale: true }), 'histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency)'),
    ];

    for (const op of SYSCALL_OPS) {
        const lower = op.toLowerCase();
        totalPlots.push(
            plot(line(op, `syscall-${lower}`, 'rate'), `sum(irate(syscall{op="${lower}"}[5m]))`),
            plot(scatter(op, `syscall-${lower}-latency`, 'time', { log_scale: true }), `histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency{op="${lower}"})`),
        );
    }

    groups.push({ name: 'Syscall', id: 'syscall', plots: totalPlots });
    return groups;
}

function generateSoftirq() {
    const groups = [];

    groups.push({
        name: 'Softirq', id: 'softirq', plots: [
            plot(line('Rate', 'softirq-total-rate', 'rate'), 'sum(irate(softirq[5m]))'),
            plot(heatmap('Rate', 'softirq-total-rate-heatmap', 'rate'), 'sum by (id) (irate(softirq[5m]))'),
            plot(line('CPU %', 'softirq-total-time', 'percentage'), 'sum(irate(softirq_time[5m])) / cpu_cores / 1000000000'),
            plot(heatmap('CPU %', 'softirq-total-time-heatmap', 'percentage'), 'sum by (id) (irate(softirq_time[5m])) / 1000000000'),
        ],
    });

    for (const [name, kind] of SOFTIRQ_KINDS) {
        groups.push({
            name, id: `softirq-${kind}`, plots: [
                plot(line('Rate', `softirq-${kind}-rate`, 'rate'), `sum(irate(softirq{kind="${kind}"}[5m]))`),
                plot(heatmap('Rate', `softirq-${kind}-rate-heatmap`, 'rate'), `sum by (id) (irate(softirq{kind="${kind}"}[5m]))`),
                plot(line('CPU %', `softirq-${kind}-time`, 'percentage'), `sum(irate(softirq_time{kind="${kind}"}[5m])) / cpu_cores / 1000000000`),
                plot(heatmap('CPU %', `softirq-${kind}-time-heatmap`, 'percentage'), `sum by (id) (irate(softirq_time{kind="${kind}"}[5m])) / 1000000000`),
            ],
        });
    }

    return groups;
}

function generateBlockIO() {
    const groups = [];

    const ops = [
        plot(line('Total Throughput', 'blockio-throughput-total', 'datarate'), 'sum(irate(blockio_bytes[5m]))'),
        plot(line('Total IOPS', 'blockio-iops-total', 'count'), 'sum(irate(blockio_operations[5m]))'),
    ];
    for (const op of ['read', 'write']) {
        const label = op.charAt(0).toUpperCase() + op.slice(1);
        ops.push(
            plot(line(`${label} Throughput`, `throughput-${op}`, 'datarate'), `sum(irate(blockio_bytes{op="${op}"}[5m]))`),
            plot(line(`${label} IOPS`, `iops-${op}`, 'count'), `sum(irate(blockio_operations{op="${op}"}[5m]))`),
        );
    }
    groups.push({ name: 'Operations', id: 'operations', plots: ops });

    const latencyPlots = [];
    for (const op of ['read', 'write']) {
        const label = op.charAt(0).toUpperCase() + op.slice(1);
        latencyPlots.push(
            plot(scatter(label, `latency-${op}`, 'time', { log_scale: true }), `histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], blockio_latency{op="${op}"})`),
        );
    }
    groups.push({ name: 'Latency', id: 'latency', plots: latencyPlots });

    const sizePlots = [];
    for (const op of ['read', 'write']) {
        const label = op.charAt(0).toUpperCase() + op.slice(1);
        sizePlots.push(
            plot(scatter(label, `size-${op}`, 'bytes', { log_scale: true }), `histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], blockio_size{op="${op}"})`),
        );
    }
    groups.push({ name: 'Size', id: 'size', plots: sizePlots });

    return groups;
}

function generateGPU() {
    const groups = [];

    groups.push({
        name: 'Utilization', id: 'utilization', plots: [
            plot(line('GPU %', 'gpu-pct', 'percentage'), 'avg(gpu_utilization) / 100'),
            plot(heatmap('GPU % (Per-GPU)', 'gpu-pct-per-gpu', 'percentage'), 'sum by (id) (gpu_utilization) / 100'),
            plot(line('Memory Controller %', 'mem-ctrl-pct', 'percentage'), 'avg(gpu_memory_utilization) / 100'),
            plot(heatmap('Memory Controller % (Per-GPU)', 'mem-ctrl-pct-per-gpu', 'percentage'), 'sum by (id) (gpu_memory_utilization) / 100'),
        ],
    });

    groups.push({
        name: 'Memory', id: 'memory', plots: [
            plot(line('Used', 'mem-used', 'bytes'), 'sum(gpu_memory{state="used"})'),
            plot(heatmap('Used (Per-GPU)', 'mem-used-per-gpu', 'bytes'), 'sum by (id) (gpu_memory{state="used"})'),
            plot(line('Free', 'mem-free', 'bytes'), 'sum(gpu_memory{state="free"})'),
            plot(heatmap('Free (Per-GPU)', 'mem-free-per-gpu', 'bytes'), 'sum by (id) (gpu_memory{state="free"})'),
            plot(line('Utilization %', 'mem-util-pct', 'percentage'), 'sum(gpu_memory{state="used"}) / (sum(gpu_memory{state="used"}) + sum(gpu_memory{state="free"}))'),
        ],
    });

    groups.push({
        name: 'Power', id: 'power', plots: [
            plot(line('Power (W)', 'power-watts', 'count', { y_axis_label: 'Watts' }), 'sum(gpu_power_usage) / 1000'),
            plot(heatmap('Power (Per-GPU)', 'power-watts-per-gpu', 'count', { y_axis_label: 'Watts' }), 'sum by (id) (gpu_power_usage) / 1000'),
            plot(line('Energy Rate (W)', 'energy-rate', 'count', { y_axis_label: 'Watts' }), 'sum(rate(gpu_energy_consumption[5m])) / 1000'),
        ],
    });

    groups.push({
        name: 'Temperature', id: 'temperature', plots: [
            plot(line('Average', 'temp-avg', 'count', { y_axis_label: '\u00b0C' }), 'avg(gpu_temperature)'),
            plot(line('Max', 'temp-max', 'count', { y_axis_label: '\u00b0C' }), 'max(gpu_temperature)'),
            plot(heatmap('Temperature (Per-GPU)', 'temp-per-gpu', 'count', { y_axis_label: '\u00b0C' }), 'sum by (id) (gpu_temperature)'),
        ],
    });

    groups.push({
        name: 'Clocks', id: 'clocks', plots: [
            plot(line('Graphics', 'clock-graphics', 'frequency'), 'avg(gpu_clock{clock="graphics"})'),
            plot(heatmap('Graphics (Per-GPU)', 'clock-graphics-per-gpu', 'frequency'), 'sum by (id) (gpu_clock{clock="graphics"})'),
            plot(line('Memory', 'clock-memory', 'frequency'), 'avg(gpu_clock{clock="memory"})'),
            plot(heatmap('Memory (Per-GPU)', 'clock-memory-per-gpu', 'frequency'), 'sum by (id) (gpu_clock{clock="memory"})'),
            plot(line('Compute', 'clock-compute', 'frequency'), 'avg(gpu_clock{clock="compute"})'),
            plot(line('Video', 'clock-video', 'frequency'), 'avg(gpu_clock{clock="video"})'),
        ],
    });

    groups.push({
        name: 'PCIe', id: 'pcie', plots: [
            plot(line('Bandwidth', 'pcie-bandwidth', 'datarate'), 'sum(gpu_pcie_bandwidth)'),
            plot(line('Receive', 'pcie-rx', 'datarate'), 'sum(gpu_pcie_throughput{direction="receive"})'),
            plot(heatmap('Receive (Per-GPU)', 'pcie-rx-per-gpu', 'datarate'), 'sum by (id) (gpu_pcie_throughput{direction="receive"})'),
            plot(line('Transmit', 'pcie-tx', 'datarate'), 'sum(gpu_pcie_throughput{direction="transmit"})'),
            plot(heatmap('Transmit (Per-GPU)', 'pcie-tx-per-gpu', 'datarate'), 'sum by (id) (gpu_pcie_throughput{direction="transmit"})'),
        ],
    });

    return groups;
}

function generateCgroups() {
    const groups = [];

    const aggPlots = [
        plot(line('Total CPU Cores', 'aggregate-total-cores', 'count'), 'sum(irate(cgroup_cpu_usage{name!~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(line('User CPU Cores', 'aggregate-user-cores', 'count'), 'sum(irate(cgroup_cpu_usage{state="user",name!~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(line('System CPU Cores', 'aggregate-system-cores', 'count'), 'sum(irate(cgroup_cpu_usage{state="system",name!~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(line('CPU Migrations', 'aggregate-cpu-migrations', 'rate'), 'sum(irate(cgroup_cpu_migrations{name!~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(line('CPU Throttled Time', 'aggregate-cpu-throttled-time', 'time'), 'sum(irate(cgroup_cpu_throttled_time{name!~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(line('IPC', 'aggregate-ipc', 'count'), 'sum(irate(cgroup_cpu_instructions{name!~"__SELECTED_CGROUPS__"}[5m])) / sum(irate(cgroup_cpu_cycles{name!~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(line('TLB Flushes', 'aggregate-tlb-flush', 'rate'), 'sum(irate(cgroup_cpu_tlb_flush{name!~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(line('Syscalls', 'aggregate-syscall', 'rate'), 'sum(irate(cgroup_syscall{name!~"__SELECTED_CGROUPS__"}[5m]))'),
    ];
    for (const op of SYSCALL_OPS) {
        const lower = op.toLowerCase();
        aggPlots.push(plot(line(`Syscall ${op}`, `aggregate-syscall-${lower}`, 'rate'), `sum(irate(cgroup_syscall{op="${lower}",name!~"__SELECTED_CGROUPS__"}[5m]))`));
    }
    groups.push({ name: 'Aggregate Cgroups', id: 'aggregate', plots: aggPlots, metadata: { side: 'left' } });

    const indPlots = [
        plot(multi('Total CPU Cores', 'individual-total-cores', 'count'), 'sum by (name) (irate(cgroup_cpu_usage{name=~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(multi('User CPU Cores', 'individual-user-cores', 'count'), 'sum by (name) (irate(cgroup_cpu_usage{state="user",name=~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(multi('System CPU Cores', 'individual-system-cores', 'count'), 'sum by (name) (irate(cgroup_cpu_usage{state="system",name=~"__SELECTED_CGROUPS__"}[5m])) / 1000000000'),
        plot(multi('CPU Migrations', 'individual-cpu-migrations', 'rate'), 'sum by (name) (irate(cgroup_cpu_migrations{name=~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(multi('CPU Throttled Time', 'individual-cpu-throttled-time', 'time'), 'sum by (name) (irate(cgroup_cpu_throttled_time{name=~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(multi('IPC', 'individual-ipc', 'count'), 'sum by (name) (irate(cgroup_cpu_instructions{name=~"__SELECTED_CGROUPS__"}[5m])) / sum by (name) (irate(cgroup_cpu_cycles{name=~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(multi('TLB Flushes', 'individual-tlb-flush', 'rate'), 'sum by (name) (irate(cgroup_cpu_tlb_flush{name=~"__SELECTED_CGROUPS__"}[5m]))'),
        plot(multi('Syscalls', 'individual-syscall', 'rate'), 'sum by (name) (irate(cgroup_syscall{name=~"__SELECTED_CGROUPS__"}[5m]))'),
    ];
    for (const op of SYSCALL_OPS) {
        const lower = op.toLowerCase();
        indPlots.push(plot(multi(`Syscall ${op}`, `individual-syscall-${lower}`, 'rate'), `sum by (name) (irate(cgroup_syscall{op="${lower}",name=~"__SELECTED_CGROUPS__"}[5m]))`));
    }
    groups.push({ name: 'Individual Cgroups', id: 'individual', plots: indPlots, metadata: { side: 'right' } });

    return groups;
}

function generateRezolus() {
    return [{
        name: 'Rezolus', id: 'rezolus', plots: [
            plot(line('CPU %', 'cpu', 'percentage'), 'sum(irate(rezolus_cpu_usage[5m])) / 1000000000'),
            plot(line('Memory (RSS)', 'memory', 'bytes'), 'sum(rezolus_memory_usage_resident_set_size)'),
            plot(line('IPC', 'ipc', 'count'), 'sum(irate(cgroup_cpu_instructions{name="/system.slice/rezolus.service"}[5m])) / sum(irate(cgroup_cpu_cycles{name="/system.slice/rezolus.service"}[5m]))'),
            plot(line('Syscalls', 'syscalls', 'rate'), 'sum(irate(cgroup_syscall{name="/system.slice/rezolus.service"}[5m]))'),
            plot(line('Total BPF Overhead', 'bpf-overhead', 'count'), 'sum(irate(rezolus_bpf_run_time[5m])) / 1000000000'),
            plot(multi('BPF Per-Sampler Overhead', 'bpf-sampler-overhead', 'count'), 'sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / 1000000000'),
            plot(multi('BPF Per-Sampler Execution Time', 'bpf-execution-time', 'time'), '(sum by (sampler) (irate(rezolus_bpf_run_time[5m])) / sum by (sampler) (irate(rezolus_bpf_run_count[5m]))) / 1000000000'),
        ],
    }];
}

function generateQueryExplorer() {
    return [];
}

const GENERATORS = {
    overview: generateOverview,
    query: generateQueryExplorer,
    cpu: generateCPU,
    gpu: generateGPU,
    memory: generateMemory,
    network: generateNetwork,
    scheduler: generateScheduler,
    syscall: generateSyscall,
    softirq: generateSoftirq,
    blockio: generateBlockIO,
    cgroups: generateCgroups,
    rezolus: generateRezolus,
};

/**
 * Generate a section's View-compatible data structure.
 * Returns { sections, groups, interval, source, version, filename, ... }
 */
export function generateSectionData(sectionKey, viewerInfo) {
    const generator = GENERATORS[sectionKey];
    if (!generator) return null;

    const groups = generator(viewerInfo);

    return {
        sections: SECTIONS,
        groups,
        interval: viewerInfo.interval,
        source: viewerInfo.source,
        version: viewerInfo.version,
        filename: viewerInfo.filename,
        start_time: viewerInfo.minTime * 1000,
        end_time: viewerInfo.maxTime * 1000,
        num_series: (viewerInfo.counter_names?.length || 0) +
                    (viewerInfo.gauge_names?.length || 0) +
                    (viewerInfo.histogram_names?.length || 0),
        metadata: sectionKey === 'cgroups' ? {
            cgroup_selector: {
                enabled: true,
                metrics: ['cgroup_cpu_usage', 'cgroup_cpu_migrations', 'cgroup_cpu_throttled_time',
                          'cgroup_cpu_throttled', 'cgroup_cpu_cycles', 'cgroup_cpu_instructions',
                          'cgroup_cpu_tlb_flush', 'cgroup_syscall'],
            },
        } : {},
    };
}

export { SECTIONS };
