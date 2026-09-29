#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod meminfo {
        include!("./linux/meminfo/stats.rs");
    }

    mod pagecache {
        include!("./linux/pagecache/stats.rs");
    }

    mod slabinfo {
        include!("./linux/slabinfo/stats.rs");
    }

    mod vmstat {
        include!("./linux/vmstat/stats.rs");
    }

    mod writeback {
        include!("./linux/writeback/stats.rs");
    }
}
