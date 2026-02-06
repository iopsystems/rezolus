//! Collects ENA network driver statistics via ethtool ioctls.
//!
//! This sampler reads NIC driver-level statistics using the same mechanism as
//! `ethtool -S`. On AWS EC2 instances with ENA interfaces, it exposes the
//! allowance exceeded counters that indicate instance-level rate limiting for
//! bandwidth, PPS, connection tracking, and link-local traffic.
//!
//! On non-ENA interfaces (or non-EC2 hosts), the sampler gracefully returns
//! `Ok(None)` at init time.

const NAME: &str = "network_ethtool";

mod stats;

use stats::*;

use crate::agent::*;

use metriken::LazyCounter;
use tokio::sync::Mutex;

use std::os::fd::RawFd;

// Ethtool ioctl command constants
const ETHTOOL_GDRVINFO: u32 = 0x00000003;
const ETHTOOL_GSTRINGS: u32 = 0x0000001b;
const ETHTOOL_GSTATS: u32 = 0x0000001d;
const ETH_SS_STATS: u32 = 0x01;
const SIOCETHTOOL: u64 = 0x8946;

// Ethtool string size
const ETH_GSTRING_LEN: usize = 32;

/// The ENA stats we want to track, mapping ethtool stat name to our metric.
const ENA_STATS: &[(&str, &LazyCounter)] = &[
    ("bw_in_allowance_exceeded", &ENA_BW_IN_ALLOWANCE_EXCEEDED),
    ("bw_out_allowance_exceeded", &ENA_BW_OUT_ALLOWANCE_EXCEEDED),
    ("pps_allowance_exceeded", &ENA_PPS_ALLOWANCE_EXCEEDED),
    (
        "conntrack_allowance_exceeded",
        &ENA_CONNTRACK_ALLOWANCE_EXCEEDED,
    ),
    (
        "linklocal_allowance_exceeded",
        &ENA_LINKLOCAL_ALLOWANCE_EXCEEDED,
    ),
];

// ---- repr(C) structs for ethtool ioctls ----

#[repr(C)]
struct EthtoolDrvinfo {
    cmd: u32,
    driver: [u8; 32],
    version: [u8; 32],
    fw_version: [u8; 32],
    bus_info: [u8; 32],
    erom_version: [u8; 32],
    reserved2: [u8; 12],
    n_priv_flags: u32,
    n_stats: u32,
    testinfo_len: u32,
    eedump_len: u32,
    regdump_len: u32,
}

impl EthtoolDrvinfo {
    fn new() -> Self {
        Self {
            cmd: ETHTOOL_GDRVINFO,
            driver: [0; 32],
            version: [0; 32],
            fw_version: [0; 32],
            bus_info: [0; 32],
            erom_version: [0; 32],
            reserved2: [0; 12],
            n_priv_flags: 0,
            n_stats: 0,
            testinfo_len: 0,
            eedump_len: 0,
            regdump_len: 0,
        }
    }
}

/// Variable-length ethtool gstrings. We heap-allocate with enough trailing
/// space for `n_strings * ETH_GSTRING_LEN` bytes.
#[repr(C)]
struct EthtoolGstringsHeader {
    cmd: u32,
    string_set: u32,
    len: u32,
    // Followed by `len * ETH_GSTRING_LEN` bytes of string data
}

/// Variable-length ethtool stats. We heap-allocate with enough trailing
/// space for `n_stats * 8` bytes (u64 values).
#[repr(C)]
struct EthtoolStatsHeader {
    cmd: u32,
    n_stats: u32,
    // Followed by `n_stats` u64 values
}

#[repr(C)]
struct Ifreq {
    ifr_name: [u8; 16],
    ifr_data: u64,
}

impl Ifreq {
    fn new(ifname: &str) -> Self {
        let mut ifr = Ifreq {
            ifr_name: [0; 16],
            ifr_data: 0,
        };
        let bytes = ifname.as_bytes();
        let len = bytes.len().min(15);
        ifr.ifr_name[..len].copy_from_slice(&bytes[..len]);
        ifr
    }
}

/// A tracked stat: its index in the ethtool stats array and the metric to update.
struct TrackedStat {
    index: usize,
    metric: &'static LazyCounter,
}

/// A tracked interface: its name and the stats we care about.
struct TrackedInterface {
    name: String,
    n_stats: u32,
    stats: Vec<TrackedStat>,
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = match EthtoolInner::new() {
        Ok(Some(inner)) => inner,
        Ok(None) => {
            debug!("{NAME}: no interfaces with ENA allowance stats found");
            return Ok(None);
        }
        Err(e) => {
            debug!("{NAME}: failed to initialize: {e}");
            return Ok(None);
        }
    };

    Ok(Some(Box::new(Ethtool {
        inner: inner.into(),
    })))
}

struct Ethtool {
    inner: Mutex<EthtoolInner>,
}

#[async_trait]
impl Sampler for Ethtool {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh();
    }
}

struct EthtoolInner {
    fd: RawFd,
    interfaces: Vec<TrackedInterface>,
}

impl Drop for EthtoolInner {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl EthtoolInner {
    pub fn new() -> anyhow::Result<Option<Self>> {
        // Open an AF_INET socket for ioctl use
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }

        let mut interfaces = Vec::new();

        // Discover network interfaces via /sys/class/net/
        let entries = match std::fs::read_dir("/sys/class/net") {
            Ok(entries) => entries,
            Err(e) => {
                unsafe {
                    libc::close(fd);
                }
                return Err(e.into());
            }
        };

        for entry in entries.flatten() {
            let ifname = entry.file_name().to_string_lossy().to_string();

            // Skip loopback
            if ifname == "lo" {
                continue;
            }

            if let Some(tracked) = Self::probe_interface(fd, &ifname)? {
                interfaces.push(tracked);
            }
        }

        if interfaces.is_empty() {
            unsafe {
                libc::close(fd);
            }
            return Ok(None);
        }

        Ok(Some(Self { fd, interfaces }))
    }

    /// Probe a single interface: get driver info, stat names, and check for ENA stats.
    fn probe_interface(fd: RawFd, ifname: &str) -> anyhow::Result<Option<TrackedInterface>> {
        // Get driver info to learn how many stats there are
        let mut drvinfo = EthtoolDrvinfo::new();
        let mut ifr = Ifreq::new(ifname);
        ifr.ifr_data = &mut drvinfo as *mut EthtoolDrvinfo as u64;

        let ret = unsafe { libc::ioctl(fd, SIOCETHTOOL, &mut ifr as *mut Ifreq) };
        if ret < 0 {
            // Interface doesn't support ethtool driver info — skip
            return Ok(None);
        }

        let n_stats = drvinfo.n_stats;
        if n_stats == 0 {
            return Ok(None);
        }

        // Get stat names
        let header_size = std::mem::size_of::<EthtoolGstringsHeader>();
        let data_size = n_stats as usize * ETH_GSTRING_LEN;
        let total_size = header_size + data_size;

        let layout = std::alloc::Layout::from_size_align(total_size, 8)
            .map_err(|e| anyhow::anyhow!("layout error: {e}"))?;

        // SAFETY: layout is valid and non-zero sized. We initialize the header
        // fields before use, and the trailing string data is written by the kernel
        // via the ioctl before we read it.
        let gstrings_ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if gstrings_ptr.is_null() {
            return Err(anyhow::anyhow!("allocation failed"));
        }

        // SAFETY: gstrings_ptr points to a valid, zeroed allocation of `total_size`
        // bytes with alignment 8, which satisfies EthtoolGstringsHeader's repr(C)
        // layout requirements.
        let header = unsafe { &mut *(gstrings_ptr as *mut EthtoolGstringsHeader) };
        header.cmd = ETHTOOL_GSTRINGS;
        header.string_set = ETH_SS_STATS;
        header.len = n_stats;

        let mut ifr = Ifreq::new(ifname);
        ifr.ifr_data = gstrings_ptr as u64;

        let ret = unsafe { libc::ioctl(fd, SIOCETHTOOL, &mut ifr as *mut Ifreq) };
        if ret < 0 {
            unsafe {
                std::alloc::dealloc(gstrings_ptr, layout);
            }
            return Ok(None);
        }

        // Parse stat names and find ENA stats
        let string_data = unsafe { gstrings_ptr.add(header_size) };

        let mut tracked_stats = Vec::new();

        for i in 0..n_stats as usize {
            let offset = i * ETH_GSTRING_LEN;
            let name_bytes =
                unsafe { std::slice::from_raw_parts(string_data.add(offset), ETH_GSTRING_LEN) };

            // Find null terminator
            let name_len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(ETH_GSTRING_LEN);
            let name = std::str::from_utf8(&name_bytes[..name_len]).unwrap_or("");

            for &(ena_name, metric) in ENA_STATS {
                if name == ena_name {
                    tracked_stats.push(TrackedStat { index: i, metric });
                    break;
                }
            }
        }

        unsafe {
            std::alloc::dealloc(gstrings_ptr, layout);
        }

        if tracked_stats.is_empty() {
            return Ok(None);
        }

        Ok(Some(TrackedInterface {
            name: ifname.to_string(),
            n_stats,
            stats: tracked_stats,
        }))
    }

    pub fn refresh(&mut self) -> Result<(), std::io::Error> {
        for iface in &self.interfaces {
            self.read_stats(iface)?;
        }

        Ok(())
    }

    fn read_stats(&self, iface: &TrackedInterface) -> Result<(), std::io::Error> {
        let header_size = std::mem::size_of::<EthtoolStatsHeader>();
        let data_size = iface.n_stats as usize * std::mem::size_of::<u64>();
        let total_size = header_size + data_size;

        let layout = std::alloc::Layout::from_size_align(total_size, 8)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // SAFETY: layout is valid and non-zero sized. We initialize the header
        // fields before use, and the trailing stat values are written by the
        // kernel via the ioctl before we read them.
        let stats_ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if stats_ptr.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "allocation failed",
            ));
        }

        // SAFETY: stats_ptr points to a valid, zeroed allocation of `total_size`
        // bytes with alignment 8, which satisfies EthtoolStatsHeader's repr(C)
        // layout requirements.
        let header = unsafe { &mut *(stats_ptr as *mut EthtoolStatsHeader) };
        header.cmd = ETHTOOL_GSTATS;
        header.n_stats = iface.n_stats;

        let mut ifr = Ifreq::new(&iface.name);
        ifr.ifr_data = stats_ptr as u64;

        let ret = unsafe { libc::ioctl(self.fd, SIOCETHTOOL, &mut ifr as *mut Ifreq) };
        if ret < 0 {
            unsafe {
                std::alloc::dealloc(stats_ptr, layout);
            }
            return Err(std::io::Error::last_os_error());
        }

        let values_ptr = unsafe { stats_ptr.add(header_size) as *const u64 };

        for stat in &iface.stats {
            let value = unsafe { *values_ptr.add(stat.index) };
            stat.metric.set(value);
        }

        unsafe {
            std::alloc::dealloc(stats_ptr, layout);
        }

        Ok(())
    }
}
