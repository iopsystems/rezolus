use crate::*;

counter_with_histogram!(
    TCP_RX_BYTES,
    TCP_RX_BYTES_HISTOGRAM,
    "tcp/receive/bytes",
    "number of bytes received over TCP"
);
counter_with_histogram!(
    TCP_RX_SEGMENTS,
    TCP_RX_SEGMENTS_HISTOGRAM,
    "tcp/receive/segments",
    "number of TCP segments received"
);
counter_with_histogram!(
    TCP_TX_BYTES,
    TCP_TX_BYTES_HISTOGRAM,
    "tcp/transmit/bytes",
    "number of bytes transmitted over TCP"
);
counter_with_histogram!(
    TCP_TX_SEGMENTS,
    TCP_TX_SEGMENTS_HISTOGRAM,
    "tcp/transmit/segments",
    "number of TCP segments transmitted"
);
counter_with_histogram!(
    TCP_TX_RETRANSMIT,
    TCP_TX_RETRANSMIT_HISTOGRAM,
    "tcp/transmit/retransmit",
    "number of TCP segments retransmitted"
);

bpfhistogram!(
    TCP_RX_SIZE,
    "tcp/receive/size",
    "distribution of receive segment sizes"
);
bpfhistogram!(
    TCP_TX_SIZE,
    "tcp/transmit/size",
    "distribution of transmit segment sizes"
);
bpfhistogram!(TCP_JITTER, "tcp/jitter");
bpfhistogram!(TCP_SRTT, "tcp/srtt");

bpfhistogram!(TCP_PACKET_LATENCY, "tcp/packet_latency");
