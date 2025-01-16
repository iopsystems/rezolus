use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the ESTABLISHED state",
    metadata = { state = "established" }
)]
pub static TCP_CONN_STATE_ESTABLISHED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the SYN_SENT state",
    metadata = { state = "syn_sent" }
)]
pub static TCP_CONN_STATE_SYN_SENT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the SYN_RECV state",
    metadata = { state = "syn_recv" }
)]
pub static TCP_CONN_STATE_SYN_RECV: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the FIN_WAIT1 state",
    metadata = { state = "fin_wait1" }
)]
pub static TCP_CONN_STATE_FIN_WAIT1: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the FIN_WAIT2 state",
    metadata = { state = "fin_wait2" }
)]
pub static TCP_CONN_STATE_FIN_WAIT2: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the TIME_WAIT state",
    metadata = { state = "time_wait" }
)]
pub static TCP_CONN_STATE_TIME_WAIT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the CLOSE state",
    metadata = { state = "close" }
)]
pub static TCP_CONN_STATE_CLOSE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the CLOSE_WAIT state",
    metadata = { state = "close_wait" }
)]
pub static TCP_CONN_STATE_CLOSE_WAIT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the LAST_ACK state",
    metadata = { state = "last_ack" }
)]
pub static TCP_CONN_STATE_LAST_ACK: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the LISTEN state",
    metadata = { state = "listen" }
)]
pub static TCP_CONN_STATE_LISTEN: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the CLOSING state",
    metadata = { state = "closing" }
)]
pub static TCP_CONN_STATE_CLOSING: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp_connection_state",
    description = "The current number of TCP connections in the NEW_SYN_RECV state",
    metadata = { state = "new_syn_recv" }
)]
pub static TCP_CONN_STATE_NEW_SYN_RECV: LazyGauge = LazyGauge::new(Gauge::default);
