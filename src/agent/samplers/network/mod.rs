#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod stats {
    mod ethtool {
        include!("./linux/ethtool/stats.rs");
    }

    mod interfaces {
        include!("./linux/interfaces/stats.rs");
    }

    mod traffic {
        include!("./linux/traffic/stats.rs");
    }
}
