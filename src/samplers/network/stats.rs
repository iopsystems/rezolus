use crate::*;
use metriken::metric;

counter_with_histogram!(
    NETWORK_RX_BYTES,
    NETWORK_RX_BYTES_HISTOGRAM,
    "network/receive/bytes",
    "number of bytes received"
);
counter_with_histogram!(
    NETWORK_RX_PACKETS,
    NETWORK_RX_PACKETS_HISTOGRAM,
    "network/receive/frames",
    "number of packets received"
);

counter_with_histogram!(
    NETWORK_TX_BYTES,
    NETWORK_TX_BYTES_HISTOGRAM,
    "network/transmit/bytes",
    "number of bytes transmitted"
);
counter_with_histogram!(
    NETWORK_TX_PACKETS,
    NETWORK_TX_PACKETS_HISTOGRAM,
    "network/transmit/packets",
    "number of packets transmitted"
);
