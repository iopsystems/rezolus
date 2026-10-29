//! TCP samplers on macOS: `tcp_mib` and `tcp_retransmit`, each from one read
//! of the kernel's TCP statistics, the `net.inet.tcp.stats` sysctl (`struct
//! tcpstat`, the counters `netstat -s -p tcp` prints).
//!
//! They use the Linux samplers' names and set the Linux metrics whose XNU
//! counter counts the same event. Metrics with no such counter are left
//! unset, and an unset lazy metric is absent from snapshots. `tcp_traffic`
//! does not run on macOS: `tcps_rcvbyte` leaves out bytes that pass through
//! reassembly, and a 32-bit byte counter can advance by 2^32 or more
//! between two reads, which happen only when the agent is scraped or a
//! stream subscriber asks for a snapshot.
//!
//! The kernel returns all zeros to a process whose effective user is not root
//! while `net.inet.tcp.disable_access_to_stats` is set, which it is by
//! default. In that case each sampler reports itself unsupported at start.
//!
//! The counters are 32 bits wide and count IPv4 and IPv6 together. Each
//! metric starts at zero when its sampler starts and adds the increase since
//! the previous read, modulo 2^32, so it loses 2^32 for each 2^32 its counter
//! advances between two reads. The counters read here count events, which
//! stay far below that at any regular scrape or stream interval; an agent
//! left unread for days can lose 2^32 on a busy counter.
//!
//! The first 32 fields of `struct tcpstat` (through `tcps_rcvshort`, 128
//! bytes) are read. The first 50 fields have the same order in every XNU
//! release from xnu-6153 (macOS 10.15) through xnu-12377 (macOS 26). Fields
//! after them have been removed and reordered between releases, which moves
//! `tcps_listendrop` and `tcps_pto`, so none of those is read.

mod mib;
mod retransmit;

use crate::agent::sampler_status::Unsupported;

/// The leading fields of XNU's `struct tcpstat` (`bsd/netinet/tcp_var.h`),
/// through `tcps_rcvshort`, the last one a sampler reads.
#[repr(C)]
#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct TcpStat {
    connattempt: u32,
    accepts: u32,
    connects: u32,
    drops: u32,
    conndrops: u32,
    closed: u32,
    segstimed: u32,
    rttupdated: u32,
    delack: u32,
    timeoutdrop: u32,
    rexmttimeo: u32,
    persisttimeo: u32,
    keeptimeo: u32,
    keepprobe: u32,
    keepdrops: u32,
    sndtotal: u32,
    sndpack: u32,
    sndbyte: u32,
    sndrexmitpack: u32,
    sndrexmitbyte: u32,
    sndacks: u32,
    sndprobe: u32,
    sndurg: u32,
    sndwinup: u32,
    sndctrl: u32,
    rcvtotal: u32,
    rcvpack: u32,
    rcvbyte: u32,
    rcvbadsum: u32,
    rcvbadoff: u32,
    rcvmemdrop: u32,
    rcvshort: u32,
}

/// One read of the TCP statistics, or `None` if the read fails or counts no
/// packet sent or received. The kernel returns zeros, successfully, when it
/// withholds the statistics, which happens if
/// `net.inet.tcp.disable_access_to_stats` is set while a process not running
/// as root reads them; taking those zeros as counts would add 2^32 to each
/// metric across the zero read and the next read with counts, since `Wide`
/// reads a fall to zero as a wrap.
fn read() -> Option<TcpStat> {
    read_raw().filter(|s| s.sndtotal != 0 || s.rcvtotal != 0)
}

/// One read of the TCP statistics, or `None` if the read fails or returns
/// fewer bytes than `TcpStat`. The kernel copies at most the length asked
/// for, so the fields after `TcpStat` are not copied.
fn read_raw() -> Option<TcpStat> {
    let mut stat = TcpStat::default();
    let mut len = std::mem::size_of::<TcpStat>();

    // SAFETY: the kernel writes at most `len` bytes into `stat`, which is
    // plain integers.
    let rc = unsafe {
        libc::sysctlbyname(
            c"net.inet.tcp.stats".as_ptr(),
            (&mut stat as *mut TcpStat).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };

    (rc == 0 && len == std::mem::size_of::<TcpStat>()).then_some(stat)
}

/// `Unsupported` if the kernel withholds the TCP statistics from this
/// process; another error if they cannot be read.
fn check() -> anyhow::Result<()> {
    if !readable_by_this_process() {
        return Err(Unsupported(
            "macOS returns zeros for the TCP statistics to a process not running as root \
             while net.inet.tcp.disable_access_to_stats is set"
                .to_string(),
        )
        .into());
    }
    // Not `read`: a host that has sent and received no TCP yet reads zeros.
    if read_raw().is_none() {
        anyhow::bail!("the net.inet.tcp.stats sysctl is not readable");
    }
    Ok(())
}

/// The kernel's check in `tcp_getstat`: the statistics are zeroed unless the
/// caller's effective user is root or `net.inet.tcp.disable_access_to_stats`
/// is 0. An unreadable setting is taken as set.
fn readable_by_this_process() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return true;
    }

    let mut value: libc::c_int = 1;
    let mut len = std::mem::size_of::<libc::c_int>();

    // SAFETY: the kernel writes at most `len` bytes into `value`.
    let rc = unsafe {
        libc::sysctlbyname(
            c"net.inet.tcp.disable_access_to_stats".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };

    rc == 0 && len == std::mem::size_of::<libc::c_int>() && value == 0
}

/// A 32-bit kernel counter as a 64-bit total: zero at the first read, then
/// each read adds the increase since the previous one, modulo 2^32.
#[derive(Default)]
struct Wide {
    last: Option<u32>,
    total: u64,
}

impl Wide {
    fn update(&mut self, value: u32) -> u64 {
        if let Some(last) = self.last {
            self.total += value.wrapping_sub(last) as u64;
        }
        self.last = Some(value);
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_starts_at_zero_and_carries_a_wrap() {
        let mut c = Wide::default();
        assert_eq!(c.update(4_000_000_000), 0);
        assert_eq!(c.update(4_000_000_100), 100);
        assert_eq!(c.update(5), 100 + (u32::MAX - 4_000_000_100) as u64 + 6);
        assert_eq!(c.update(5), 100 + (u32::MAX - 4_000_000_100) as u64 + 6);
    }

    // The offsets XNU's header gives these fields; a missing or extra field
    // above them would move every one after it.
    #[test]
    fn tcp_stat_layout_matches_xnu() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(TcpStat, delack), 32);
        assert_eq!(offset_of!(TcpStat, rexmttimeo), 40);
        assert_eq!(offset_of!(TcpStat, sndtotal), 60);
        assert_eq!(offset_of!(TcpStat, sndrexmitpack), 72);
        assert_eq!(offset_of!(TcpStat, rcvtotal), 100);
        assert_eq!(offset_of!(TcpStat, rcvbadsum), 112);
        assert_eq!(offset_of!(TcpStat, rcvbadoff), 116);
        assert_eq!(offset_of!(TcpStat, rcvshort), 124);
        assert_eq!(std::mem::size_of::<TcpStat>(), 128);
    }

    // `read_raw` succeeds with or without access. Without access (not root,
    // `disable_access_to_stats` set) the kernel returns zeros, which `read`
    // refuses; with access it returns counts.
    #[test]
    fn read_refuses_the_zeros_the_kernel_returns_without_access() {
        assert!(read_raw().is_some());
        assert_eq!(read().is_some(), readable_by_this_process());
    }
}
