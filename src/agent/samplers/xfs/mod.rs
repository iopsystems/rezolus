#[cfg(target_os = "linux")]
mod linux;

// Non-Linux builds still need metric and acquisition-group registration.
#[cfg(not(target_os = "linux"))]
mod stats {
    #[allow(clippy::module_inception)]
    mod stats {
        include!("./linux/stats/stats.rs");
    }
}
