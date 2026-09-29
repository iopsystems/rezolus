#[cfg(target_os = "linux")]
mod linux;

// Non-Linux builds still need metric and acquisition-group registration.
#[cfg(not(target_os = "linux"))]
mod stats {
    mod alloc {
        include!("./linux/alloc/stats.rs");
    }

    mod journal {
        include!("./linux/journal/stats.rs");
    }
}
