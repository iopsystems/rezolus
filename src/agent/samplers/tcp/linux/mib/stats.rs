use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `tcp/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s procfs reader is
// Linux-only.
//
// Two read sections, one per file: each is its own read of its own source and
// fails on its own, so each has its own window.

/// Brackets the `/proc/net/netstat` read + parse of its `TcpExt` section
/// (single writer: this sampler's own `refresh()`).
pub static NETSTAT_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("tcp_mib"),
    "tcp_mib_netstat_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static NETSTAT_ACQ_REG: &'static AcquisitionGroup = &NETSTAT_ACQ;

/// Brackets the `/proc/net/snmp` read + parse of its `Tcp` section (single
/// writer: this sampler's own `refresh()`).
pub static SNMP_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("tcp_mib"),
    "tcp_mib_snmp_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SNMP_ACQ_REG: &'static AcquisitionGroup = &SNMP_ACQ;

/*
 * /proc/net/netstat, TcpExt section
 */

/*
 * ACKs
 */

#[metric(
    name = "tcp_delayed_acks",
    description = "Delayed-ACK timer firings that sent an ACK (DelayedACKs): an ACK held back on receive that no outgoing segment had carried by the time the timer fired. Includes firings first deferred because the socket was owned by the application",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DELAYED_ACKS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_delayed_ack_deferrals",
    description = "Delayed-ACK timer firings that found the socket owned by the application, in a system call, and deferred the ACK until the application released it (DelayedACKLocked)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DELAYED_ACK_DEFERRALS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_duplicate_data_segments",
    description = "Data segments received that start below the next expected sequence number, so carry data already received (DelayedACKLost). The kernel takes each as a sign that the peer did not see the ACK for that data in time, and switches to immediate ACKs",
    metadata = { unit = "segments", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DUPLICATE_DATA_SEGMENTS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * retransmission and loss recovery
 */

#[metric(
    name = "tcp_rto_expirations",
    description = "Retransmission timer (RTO) expirations, plus SYN-ACK timeouts on pending connection requests (TCPTimeouts). An expiration while the peer advertises a zero window is not counted",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_RTO_EXPIRATIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_loss_probes",
    description = "Tail loss probes sent when the probe timeout fired (TCPLossProbes): new data, or a retransmission of the last segment, sent to provoke an ACK before an RTO would fire",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LOSS_PROBES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_loss_probe_recoveries",
    description = "Losses repaired by a tail loss probe (TCPLossProbeRecovery): an ACK advanced past the probe without a D-SACK for it, so the congestion window was reduced instead of waiting for an RTO",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LOSS_PROBE_RECOVERIES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_loss_retransmits",
    description = "Segments marked lost and retransmitted from the retransmit queue outside the loss state, during fast recovery (TCPFastRetrans)",
    metadata = { unit = "segments", kind = "fast", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LOSS_RETRANSMITS_FAST: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_loss_retransmits",
    description = "Segments marked lost and retransmitted from the retransmit queue in the loss state entered on an RTO (TCPSlowStartRetrans)",
    metadata = { unit = "segments", kind = "slow_start", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LOSS_RETRANSMITS_SLOW_START: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_retransmits_lost",
    description = "Retransmitted segments that were detected lost again (TCPLostRetransmit)",
    metadata = { unit = "segments", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_RETRANSMITS_LOST: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_spurious_rtos",
    description = "RTOs that F-RTO found to be spurious, with the congestion window reduction undone (TCPSpuriousRTOs)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_SPURIOUS_RTOS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_fast_recoveries",
    description = "Entries into fast recovery on connections that negotiated SACK (TCPSackRecovery)",
    metadata = { unit = "events", kind = "sack", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_FAST_RECOVERIES_SACK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_fast_recoveries",
    description = "Entries into fast recovery on connections without SACK (TCPRenoRecovery)",
    metadata = { unit = "events", kind = "reno", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_FAST_RECOVERIES_RENO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_dsacks_received",
    description = "ACKs received whose first SACK block is a D-SACK below the cumulative ACK (TCPDSACKRecv): the peer received a segment twice, usually because a retransmission was unnecessary",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DSACKS_RECEIVED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_dsacks_sent",
    description = "D-SACKs sent for duplicate data below the next expected sequence number (TCPDSACKOldSent)",
    metadata = { unit = "events", kind = "old", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DSACKS_SENT_OLD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_dsacks_sent",
    description = "D-SACKs sent for duplicate data at or above the next expected sequence number, already held out of order (TCPDSACKOfoSent)",
    metadata = { unit = "events", kind = "out_of_order", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_DSACKS_SENT_OUT_OF_ORDER: LazyCounter = LazyCounter::new(Counter::default);

/*
 * receive window
 */

#[metric(
    name = "tcp_zero_window_transitions",
    description = "Segments sent advertising a zero receive window where the previous advertisement was non-zero (TCPToZeroWindowAdv)",
    metadata = { unit = "segments", kind = "closed", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ZERO_WINDOW_CLOSED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_zero_window_transitions",
    description = "Segments sent advertising a non-zero receive window where the previous advertisement was zero (TCPFromZeroWindowAdv)",
    metadata = { unit = "segments", kind = "reopened", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ZERO_WINDOW_REOPENED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_zero_window_withheld",
    description = "Window selections that computed a zero receive window but advertised the current window instead, because the kernel does not shrink a window it has offered (TCPWantZeroWindowAdv)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ZERO_WINDOW_WITHHELD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_zero_window_drops",
    description = "Data segments dropped because the receive window was zero (TCPZeroWindowDrop)",
    metadata = { unit = "segments", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ZERO_WINDOW_DROPS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * receive memory
 */

#[metric(
    name = "tcp_prune_attempts",
    description = "Times the receive path ran the queue pruner because a socket could not be charged memory for an incoming segment (PruneCalled)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_PRUNE_ATTEMPTS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_prune_failures",
    description = "Prune runs that could not bring a socket's receive memory under its receive buffer limit even after collapsing queues and discarding out-of-order data, so the incoming segment was dropped (RcvPruned)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_PRUNE_FAILURES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_out_of_order_prunes",
    description = "Prune runs that discarded out-of-order data to free receive memory (OfoPruned). The discarded data must be retransmitted",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_OUT_OF_ORDER_PRUNES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_collapsed_buffers",
    description = "Socket buffers freed by copying receive or out-of-order queue data into fewer, fuller buffers to reduce memory overhead (TCPRcvCollapsed)",
    metadata = { unit = "buffers", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_COLLAPSED_BUFFERS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_backlog_drops",
    description = "Segments dropped because the socket was owned by the application, in a system call, and its backlog queue was over its limit (TCPBacklogDrop)",
    metadata = { unit = "segments", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_BACKLOG_DROPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_rcvbuf_drops",
    description = "In-order segments dropped because the socket could not be charged the memory to queue them and its receive queue was not empty (TCPRcvQDrop)",
    metadata = { unit = "segments", kind = "in_order", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_RCVBUF_DROPS_IN_ORDER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_rcvbuf_drops",
    description = "Out-of-order segments dropped because the socket could not be charged the memory to queue them (TCPOFODrop)",
    metadata = { unit = "segments", kind = "out_of_order", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_RCVBUF_DROPS_OUT_OF_ORDER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_out_of_order_queued",
    description = "Segments placed in the out-of-order queue because they arrived ahead of the next expected sequence number (TCPOFOQueue). A GRO-merged buffer counts once",
    metadata = { unit = "segments", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_OUT_OF_ORDER_QUEUED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_memory_pressure_entries",
    description = "Times TCP as a whole entered memory pressure because its total memory passed the tcp_mem pressure threshold (TCPMemoryPressures). Counted on entry only",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_MEMORY_PRESSURE_ENTRIES: LazyCounter = LazyCounter::new(Counter::default);

/*
 * send path
 */

#[metric(
    name = "tcp_autocorks",
    description = "Times a send held back a small segment because earlier data from the same socket was still queued in the qdisc or device (TCPAutoCorking). Counted when the socket enters that state, not per write",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_AUTOCORKS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * listeners
 */

#[metric(
    name = "tcp_listen_overflows",
    description = "SYNs and handshake-completing ACKs dropped because the listener's accept queue was full (ListenOverflows)",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LISTEN_OVERFLOWS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_listen_drops",
    description = "Connection requests and handshake-completing ACKs a listener dropped for any reason (ListenDrops): accept-queue overflows, most of which are also in tcp_listen_overflows, plus allocation, routing and other failures",
    metadata = { unit = "events", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_LISTEN_DROPS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * aborts
 */

#[metric(
    name = "tcp_aborts",
    description = "Connections reset because data or an out-of-order FIN arrived after the application closed the socket, or closed with SO_LINGER and a zero timeout (TCPAbortOnData)",
    metadata = { unit = "connections", reason = "data", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ABORTS_DATA: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_aborts",
    description = "Connections dropped because retransmissions, zero-window probes, keepalive probes or SYN-ACK retries ran out (TCPAbortOnTimeout)",
    metadata = { unit = "connections", reason = "timeout", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ABORTS_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_aborts",
    description = "Orphaned connections killed because there were too many orphans or TCP was under memory pressure (TCPAbortOnMemory)",
    metadata = { unit = "connections", reason = "memory", acq_group = "tcp_mib_netstat_read" }
)]
pub static TCP_ABORTS_MEMORY: LazyCounter = LazyCounter::new(Counter::default);

/*
 * /proc/net/snmp, Tcp section
 */

#[metric(
    name = "tcp_open_failures",
    description = "Connection attempts that failed (AttemptFails): moves from SYN-SENT or SYN-RECV to CLOSED, and pending passive opens reset by an RST or SYN",
    metadata = { unit = "connections", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_OPEN_FAILURES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_established_resets",
    description = "Connections moved to CLOSED directly from ESTABLISHED or CLOSE-WAIT, without the FIN handshake: resets received or sent, and aborts (EstabResets)",
    metadata = { unit = "connections", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_ESTABLISHED_RESETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_input_errors",
    description = "Segments received in error (InErrs): a bad checksum, a header shorter than its data offset, or certain invalid SYNs",
    metadata = { unit = "segments", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_INPUT_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_checksum_errors",
    description = "Segments received with a bad checksum (InCsumErrors). Also counted in tcp_input_errors",
    metadata = { unit = "segments", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_CHECKSUM_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_resets_sent",
    description = "Segments sent with the RST flag, from sockets and from stateless replies to segments that matched no socket (OutRsts)",
    metadata = { unit = "segments", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_RESETS_SENT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_segments_retransmitted",
    description = "Segments retransmitted, counted in TSO/GSO segments, including tail loss probes that retransmit and SYN-ACK retransmissions (RetransSegs). Counted when the retransmission is handed to the transmit path, before it is known to have been sent",
    metadata = { unit = "segments", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_SEGMENTS_RETRANSMITTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_established_connections",
    description = "Connections currently in ESTABLISHED or CLOSE-WAIT (CurrEstab)",
    metadata = { unit = "connections", acq_group = "tcp_mib_snmp_read" }
)]
pub static TCP_ESTABLISHED_CONNECTIONS: LazyGauge = LazyGauge::new(Gauge::default);
