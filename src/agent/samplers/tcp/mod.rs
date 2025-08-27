#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
	mod connect_latency {
		include!("./linux/connect_latency/stats.rs");
	}
    
    mod packet_latency {
    	include!("./linux/packet_latency/stats.rs");
    }

    mod receive {
    	include!("./linux/receive/stats.rs");
    }

    mod retransmit {
    	include!("./linux/retransmit/stats.rs");
    }

    mod traffic {
    	include!("./linux/traffic/stats.rs");
    }
}
