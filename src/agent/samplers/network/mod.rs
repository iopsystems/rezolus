#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod ethtool {
        include!("./linux/ethtool/stats.rs");
    }

    mod interfaces {
        include!("./linux/interfaces/stats.rs");
    }

    #[cfg(not(target_os = "macos"))]
    mod traffic {
        include!("./linux/traffic/stats.rs");
    }
}
