#[cfg(target_os = "linux")]
mod linux;

/// The mount-table parser, shared with the per-filesystem BPF slot registry
/// (`crate::agent::bpf::filesystems`).
#[cfg(target_os = "linux")]
pub(crate) use linux::mounts;

#[cfg(target_os = "macos")]
mod macos;

// Other builds still need metric and acquisition-group registration.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stats {
    include!("./linux/stats.rs");
}
