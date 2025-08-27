#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod meminfo {
        include!("./linux/meminfo/stats.rs");
    }

    mod vmstat {
        include!("./linux/vmstat/stats.rs");
    }
}
