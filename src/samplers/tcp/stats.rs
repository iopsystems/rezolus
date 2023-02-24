use crate::*;



type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

counter_with_heatmap!(TCP_RX_SEGS, TCP_RX_SEGS_HIST, "tcp/receive/segments", "number of TCP segments received");
counter_with_heatmap!(TCP_TX_SEGS, TCP_TX_SEGS_HIST, "tcp/transmit/segments", "number of TCP segments transmitted");

counter!(SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE, "samplers/tcp/classic/snmp/sample");
counter!(SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE_EX, "samplers/tcp/classic/snmp/sample_ex");
