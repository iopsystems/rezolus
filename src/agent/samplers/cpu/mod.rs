#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "linux"))]
pub mod stats {
    mod cores {
        include!("./linux/cores/stats.rs");
    }

    mod bandwidth {
        include!("./linux/bandwidth/stats.rs");
    }

    mod frequency {
        include!("./linux/frequency/stats.rs");
    }

    mod l3 {
        include!("./linux/l3/stats.rs");
    }

    mod migrations {
        include!("./linux/migrations/stats.rs");
    }

    mod perf {
        include!("./linux/perf/stats.rs");
    }

    mod tlb_flush {
        include!("./linux/tlb_flush/stats.rs");
    }

    mod usage {
        include!("./linux/usage/stats.rs");
    }
}
