#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

// Where a sampler does not exist, its metric definitions are still compiled
// so exposition and dashboards stay consistent across platforms. macOS has a
// `blockio_requests` sampler (in `macos/`) but no `blockio_latency` one.
#[cfg(not(target_os = "linux"))]
mod stats {
    mod latency {
        include!("./linux/latency/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod requests {
        include!("./linux/requests/stats.rs");
    }
}
