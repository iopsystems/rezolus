#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod nvidia {
        include!("./linux/nvidia/stats.rs");
    }
}
