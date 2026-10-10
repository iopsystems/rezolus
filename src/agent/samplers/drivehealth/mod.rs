#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod nvme_log;

// Where there is no sampler, the metric definitions are still compiled so
// exposition/dashboards stay consistent across platforms (matches the other
// Linux-only samplers, e.g. blockio, scheduler).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stats {
    include!("./linux/stats.rs");
}
