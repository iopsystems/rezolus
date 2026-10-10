#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stats {
    include!("./linux/blockio/stats.rs");
}
