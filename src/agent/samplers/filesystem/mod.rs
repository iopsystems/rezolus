#[cfg(target_os = "linux")]
mod linux;

/// The mount-table parser, shared with the per-filesystem BPF slot registry
/// (`crate::agent::bpf::filesystems`).
#[cfg(target_os = "linux")]
pub(crate) use linux::mounts;

// Non-Linux builds still need metric and acquisition-group registration.
#[cfg(not(target_os = "linux"))]
mod stats {
    include!("./linux/stats.rs");
}
