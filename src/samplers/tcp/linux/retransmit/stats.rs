use metriken::*;

#[metric(
    name = "tcp_transmit_retransmit",
    description = "The number of TCP packets that were re-transmitted",
    metadata = { unit = "packets" }
)]
pub static TCP_TX_RETRANSMIT: LazyCounter = LazyCounter::new(Counter::default);
