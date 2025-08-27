#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod runqueue {
        include!("./linux/runqueue/stats.rs");
    }
}
