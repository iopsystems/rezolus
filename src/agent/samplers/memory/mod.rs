#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "linux"))]
mod stats {
    #[cfg(not(target_os = "macos"))]
    mod meminfo {
        include!("./linux/meminfo/stats.rs");
    }

    mod pagecache {
        include!("./linux/pagecache/stats.rs");
    }

    mod slabinfo {
        include!("./linux/slabinfo/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod vmstat {
        include!("./linux/vmstat/stats.rs");
    }

    mod writeback {
        include!("./linux/writeback/stats.rs");
    }
}
