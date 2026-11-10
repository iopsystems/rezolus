use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Upper bound on EFA ports tracked. An EFA device has one port
/// (`phys_port_cnt = 1` in the driver), and 64 is above the EFA device count of
/// any instance type we know of.
pub const MAX_PORTS: usize = 64;

/// Upper bound on EFA devices tracked.
pub const MAX_DEVICES: usize = 64;

/// The port counters, one member per port: every `ports/<n>/hw_counters`
/// file this sampler reads. Single writer: the `spawn_blocking` sweep in
/// `mod.rs`, which reads the device counters in the same pass.
pub static NETWORK_EFA_PORTS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "network_efa_ports");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static NETWORK_EFA_PORTS_ACQ_REG: &'static AcquisitionGroup = &NETWORK_EFA_PORTS_ACQ;

/// The device counters, one member per device: the `hw_counters` files of
/// the device itself. A separate group because a slot means a port in the
/// other and a device here.
pub static NETWORK_EFA_DEVICES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "network_efa_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static NETWORK_EFA_DEVICES_ACQ_REG: &'static AcquisitionGroup = &NETWORK_EFA_DEVICES_ACQ;

#[metric(
    name = "network_efa_tx_bytes",
    description = "Bytes transmitted by an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_TX_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_tx_pkts",
    description = "Packets transmitted by an EFA port.",
    metadata = { unit = "packets", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_TX_PKTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rx_bytes",
    description = "Bytes received by an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RX_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rx_pkts",
    description = "Packets received by an EFA port.",
    metadata = { unit = "packets", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RX_PKTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rx_drops",
    description = "Packets an EFA port dropped on receive.",
    metadata = { unit = "packets", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RX_DROPS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_send_bytes",
    description = "Bytes in send work requests (two-sided messages) on an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_SEND_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_send_wrs",
    description = "Send work requests on an EFA port.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_SEND_WRS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_recv_bytes",
    description = "Bytes in receive work requests (two-sided messages) on an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RECV_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_recv_wrs",
    description = "Receive work requests on an EFA port.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RECV_WRS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_read_wrs",
    description = "RDMA read work requests this host issued on an EFA port.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_READ_WRS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_read_bytes",
    description = "Bytes read by RDMA reads this host issued on an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_READ_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_read_wr_err",
    description = "RDMA read work requests this host issued on an EFA port that completed with an error.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_READ_WR_ERR: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_read_resp_bytes",
    description = "Bytes an EFA port sent in response to RDMA reads issued by remote hosts.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_READ_RESP_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_write_wrs",
    description = "RDMA write work requests this host issued on an EFA port.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_WRITE_WRS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_write_bytes",
    description = "Bytes written by RDMA writes this host issued on an EFA port.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_WRITE_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_write_wr_err",
    description = "RDMA write work requests this host issued on an EFA port that completed with an error.",
    metadata = { unit = "operations", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_WRITE_WR_ERR: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_rdma_write_recv_bytes",
    description = "Bytes an EFA port received from RDMA writes issued by remote hosts.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RDMA_WRITE_RECV_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_retrans_bytes",
    description = "Bytes an EFA port retransmitted.",
    metadata = { unit = "bytes", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RETRANS_BYTES: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_retrans_pkts",
    description = "Packets an EFA port retransmitted.",
    metadata = { unit = "packets", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RETRANS_PKTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_retrans_timeout_events",
    description = "Retransmission timeouts on an EFA port.",
    metadata = { unit = "events", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_RETRANS_TIMEOUT_EVENTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_unresponsive_remote_events",
    description = "Events an EFA port counted as a remote host not responding.",
    metadata = { unit = "events", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_UNRESPONSIVE_REMOTE_EVENTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_impaired_remote_conn_events",
    description = "Events an EFA port counted as an impaired connection to a remote host.",
    metadata = { unit = "events", acq_group = "network_efa_ports" }
)]
pub static NETWORK_EFA_IMPAIRED_REMOTE_CONN_EVENTS: CounterGroup = CounterGroup::new(MAX_PORTS);

#[metric(
    name = "network_efa_cmds_err",
    description = "Admin commands to an EFA device that completed with an error.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_CMDS_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_no_completion_cmds",
    description = "Admin commands to an EFA device that got no completion.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_NO_COMPLETION_CMDS: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_alloc_pd_err",
    description = "Protection domain allocations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_ALLOC_PD_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_alloc_mr_err",
    description = "Memory region allocations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_ALLOC_MR_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_alloc_ucontext_err",
    description = "User context allocations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_ALLOC_UCONTEXT_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_create_ah_err",
    description = "Address handle creations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_CREATE_AH_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_create_cq_err",
    description = "Completion queue creations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_CREATE_CQ_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_create_qp_err",
    description = "Queue pair creations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_CREATE_QP_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_get_dma_mr_err",
    description = "DMA memory region requests on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_GET_DMA_MR_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_mmap_err",
    description = "Memory mappings of an EFA device's queues that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_MMAP_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

#[metric(
    name = "network_efa_reg_mr_err",
    description = "Memory region registrations on an EFA device that failed.",
    metadata = { unit = "operations", acq_group = "network_efa_devices" }
)]
pub static NETWORK_EFA_REG_MR_ERR: CounterGroup = CounterGroup::new(MAX_DEVICES);

/// Each port counter's `hw_counters` file name and metric.
pub static PORT_COUNTERS: &[(&str, &CounterGroup)] = &[
    ("tx_bytes", &NETWORK_EFA_TX_BYTES),
    ("tx_pkts", &NETWORK_EFA_TX_PKTS),
    ("rx_bytes", &NETWORK_EFA_RX_BYTES),
    ("rx_pkts", &NETWORK_EFA_RX_PKTS),
    ("rx_drops", &NETWORK_EFA_RX_DROPS),
    ("send_bytes", &NETWORK_EFA_SEND_BYTES),
    ("send_wrs", &NETWORK_EFA_SEND_WRS),
    ("recv_bytes", &NETWORK_EFA_RECV_BYTES),
    ("recv_wrs", &NETWORK_EFA_RECV_WRS),
    ("rdma_read_wrs", &NETWORK_EFA_RDMA_READ_WRS),
    ("rdma_read_bytes", &NETWORK_EFA_RDMA_READ_BYTES),
    ("rdma_read_wr_err", &NETWORK_EFA_RDMA_READ_WR_ERR),
    ("rdma_read_resp_bytes", &NETWORK_EFA_RDMA_READ_RESP_BYTES),
    ("rdma_write_wrs", &NETWORK_EFA_RDMA_WRITE_WRS),
    ("rdma_write_bytes", &NETWORK_EFA_RDMA_WRITE_BYTES),
    ("rdma_write_wr_err", &NETWORK_EFA_RDMA_WRITE_WR_ERR),
    ("rdma_write_recv_bytes", &NETWORK_EFA_RDMA_WRITE_RECV_BYTES),
    ("retrans_bytes", &NETWORK_EFA_RETRANS_BYTES),
    ("retrans_pkts", &NETWORK_EFA_RETRANS_PKTS),
    (
        "retrans_timeout_events",
        &NETWORK_EFA_RETRANS_TIMEOUT_EVENTS,
    ),
    (
        "unresponsive_remote_events",
        &NETWORK_EFA_UNRESPONSIVE_REMOTE_EVENTS,
    ),
    (
        "impaired_remote_conn_events",
        &NETWORK_EFA_IMPAIRED_REMOTE_CONN_EVENTS,
    ),
];

/// Each device counter's `hw_counters` file name and metric.
pub static DEVICE_COUNTERS: &[(&str, &CounterGroup)] = &[
    ("cmds_err", &NETWORK_EFA_CMDS_ERR),
    ("no_completion_cmds", &NETWORK_EFA_NO_COMPLETION_CMDS),
    ("alloc_pd_err", &NETWORK_EFA_ALLOC_PD_ERR),
    ("alloc_mr_err", &NETWORK_EFA_ALLOC_MR_ERR),
    ("alloc_ucontext_err", &NETWORK_EFA_ALLOC_UCONTEXT_ERR),
    ("create_ah_err", &NETWORK_EFA_CREATE_AH_ERR),
    ("create_cq_err", &NETWORK_EFA_CREATE_CQ_ERR),
    ("create_qp_err", &NETWORK_EFA_CREATE_QP_ERR),
    ("get_dma_mr_err", &NETWORK_EFA_GET_DMA_MR_ERR),
    ("mmap_err", &NETWORK_EFA_MMAP_ERR),
    ("reg_mr_err", &NETWORK_EFA_REG_MR_ERR),
];

/// The port counters, as the slot-identity group that labels each port.
pub static PORT_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &NETWORK_EFA_TX_BYTES,
    &NETWORK_EFA_TX_PKTS,
    &NETWORK_EFA_RX_BYTES,
    &NETWORK_EFA_RX_PKTS,
    &NETWORK_EFA_RX_DROPS,
    &NETWORK_EFA_SEND_BYTES,
    &NETWORK_EFA_SEND_WRS,
    &NETWORK_EFA_RECV_BYTES,
    &NETWORK_EFA_RECV_WRS,
    &NETWORK_EFA_RDMA_READ_WRS,
    &NETWORK_EFA_RDMA_READ_BYTES,
    &NETWORK_EFA_RDMA_READ_WR_ERR,
    &NETWORK_EFA_RDMA_READ_RESP_BYTES,
    &NETWORK_EFA_RDMA_WRITE_WRS,
    &NETWORK_EFA_RDMA_WRITE_BYTES,
    &NETWORK_EFA_RDMA_WRITE_WR_ERR,
    &NETWORK_EFA_RDMA_WRITE_RECV_BYTES,
    &NETWORK_EFA_RETRANS_BYTES,
    &NETWORK_EFA_RETRANS_PKTS,
    &NETWORK_EFA_RETRANS_TIMEOUT_EVENTS,
    &NETWORK_EFA_UNRESPONSIVE_REMOTE_EVENTS,
    &NETWORK_EFA_IMPAIRED_REMOTE_CONN_EVENTS,
]];

/// The device counters, as the slot-identity group that labels each device.
pub static DEVICE_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &NETWORK_EFA_CMDS_ERR,
    &NETWORK_EFA_NO_COMPLETION_CMDS,
    &NETWORK_EFA_ALLOC_PD_ERR,
    &NETWORK_EFA_ALLOC_MR_ERR,
    &NETWORK_EFA_ALLOC_UCONTEXT_ERR,
    &NETWORK_EFA_CREATE_AH_ERR,
    &NETWORK_EFA_CREATE_CQ_ERR,
    &NETWORK_EFA_CREATE_QP_ERR,
    &NETWORK_EFA_GET_DMA_MR_ERR,
    &NETWORK_EFA_MMAP_ERR,
    &NETWORK_EFA_REG_MR_ERR,
]];
