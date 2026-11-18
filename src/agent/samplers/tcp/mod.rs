#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod connect_latency {
        include!("./linux/connect_latency/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod mib {
        include!("./linux/mib/stats.rs");
    }

    mod packet_latency {
        include!("./linux/packet_latency/stats.rs");
    }

    mod receive {
        include!("./linux/receive/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod retransmit {
        include!("./linux/retransmit/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod traffic {
        include!("./linux/traffic/stats.rs");
    }
}
