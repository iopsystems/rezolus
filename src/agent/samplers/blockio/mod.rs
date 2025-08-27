#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod latency {
        include!("./linux/latency/stats.rs");
    }

    mod requests {
        include!("./linux/requests/stats.rs");
    }
}
