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

    // macOS compiles these two in its own samplers.
    #[cfg(not(target_os = "macos"))]
    mod frequency {
        include!("./linux/frequency/stats.rs");
    }

    mod l3 {
        include!("./linux/l3/stats.rs");
    }

    mod perf {
        include!("./linux/perf/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod power {
        include!("./linux/power/stats.rs");
    }

    mod tlb_flush {
        include!("./linux/tlb_flush/stats.rs");
    }

    mod usage {
        include!("./linux/usage/stats.rs");
    }
}
