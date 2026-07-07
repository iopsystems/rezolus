#[cfg(target_os = "linux")]
mod linux;

// On non-Linux the sampler does not run, but the metric definition is still
// compiled so exposition/dashboards stay consistent across platforms (matches
// the other Linux-only samplers, e.g. blockio, scheduler).
#[cfg(not(target_os = "linux"))]
mod stats {
    include!("./linux/stats.rs");
}
