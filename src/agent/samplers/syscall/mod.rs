#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod counts {
        include!("./linux/counts/stats.rs");
    }

    mod latency {
        include!("./linux/latency/stats.rs");
    }
}
