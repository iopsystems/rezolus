use crate::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

counter_with_heatmap!(TCP_RX_BYTES, TCP_RX_BYTES_HEATMAP, "tcp/receive/bytes", "number of bytes received over TCP");
counter_with_heatmap!(TCP_RX_SEGMENTS, TCP_RX_SEGMENTS_HEATMAP, "tcp/receive/segments", "number of TCP segments received");
counter_with_heatmap!(TCP_TX_BYTES, TCP_TX_BYTES_HEATMAP, "tcp/transmit/bytes", "number of bytes transmitted over TCP");
counter_with_heatmap!(TCP_TX_SEGMENTS, TCP_TX_SEGMENTS_HEATMAP, "tcp/transmit/segments", "number of TCP segments transmitted");
counter_with_heatmap!(TCP_TX_RETRANSMIT, TCP_TX_RETRANSMIT_HEATMAP, "tcp/transmit/retransmit", "number of TCP segments retransmitted");

heatmap!(TCP_RX_SIZE, "tcp/receive/size", "distribution of receive segment sizes");
heatmap!(TCP_TX_SIZE, "tcp/transmit/size", "distribution of transmit segment sizes");
heatmap!(TCP_JITTER, "tcp/jitter");
heatmap!(TCP_SRTT, "tcp/srtt");

// counter!(SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE, "samplers/tcp/classic/snmp/sample");
// counter!(SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE_EX, "samplers/tcp/classic/snmp/sample_ex");

// counter!(SAMPLERS_TCP_BPF_TRAFFIC_SAMPLE, "samplers/tcp/bpf/traffic/sample");
