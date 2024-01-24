use crate::*;

sampler!(Tcp, "tcp", TCP_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
#[path = "."]
mod linux {
    mod connection_state;
    mod snmp;

    #[cfg(feature = "bpf")]
    #[path = "."]
    mod bpf {
        mod packet_latency;
        mod receive;
        mod retransmit;
        mod traffic;
    }
}
