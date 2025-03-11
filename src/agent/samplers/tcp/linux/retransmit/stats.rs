use metriken::*;

#[metric(
    name = "tcp_retransmit",
    description = "The number of TCP packets that were re-transmitted",
    metadata = { unit = "packets" }
)]
pub static TCP_RETRANSMIT: LazyCounter = LazyCounter::new(Counter::default);
