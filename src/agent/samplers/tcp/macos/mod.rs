//! TCP samplers on macOS: `tcp_mib`, `tcp_retransmit` and `tcp_traffic`,
//! from the kernel's TCP statistics, the `net.inet.tcp.stats` sysctl
//! (`struct tcpstat`, the counters `netstat -s -p tcp` prints).
//!
//! They use the Linux samplers' names and set the Linux metrics whose XNU
//! counter counts the same event, or for `tcp_bytes` the closest sum.
//! Metrics with no such counter are left unset, and an unset lazy metric is
//! absent from snapshots.
//!
//! The kernel returns all zeros to a process whose effective user is not root
//! while `net.inet.tcp.disable_access_to_stats` is set, which it is by
//! default. In that case each sampler reports itself unsupported at start.
//!
//! The counters are 32 bits wide and count IPv4 and IPv6 together. One set
//! of 64-bit totals, `COUNTERS`, is shared by the three samplers: each read
//! adds every counter's increase since the previous read, modulo 2^32. The
//! kernel updates the counters without atomics, so two CPUs adding to one
//! counter at once can leave a read that is higher than the next one; `Wide`
//! therefore takes a fall of less than `MAX_BACKWARD` (256 MiB, 2^28) as
//! such a backward step and adds nothing. A total is exact while its counter
//! advances by less than 2^32 - 2^28 between two reads. A larger advance
//! loses a multiple of 2^32, and when it ends within 2^28 short of a multiple
//! of 2^32 the total holds until the counter advances by up to 256 MiB more.
//! The totals start at zero at the first read after the agent starts.
//!
//! Each sampler's `refresh()` reads the sysctl, so its window brackets a read
//! of its own. Reads otherwise happen only when the agent is scraped or a
//! stream subscriber asks for a snapshot, and a byte counter advances by
//! 2^32 in 3.4 seconds at 10 Gbit/s. While `tcp_traffic` runs, a thread
//! therefore also reads every `WIDEN_INTERVAL` (250 ms), which keeps the
//! totals exact below 128 Gbit/s, less when the thread is delayed. That
//! thread is the agent's own clock, a departure from principle 10
//! (docs/principles.md): it updates the totals and sets no metric. Without
//! `tcp_traffic`, the event counters `tcp_mib` and `tcp_retransmit` read stay
//! far below 2^32 between reads at any regular scrape or stream interval; an
//! agent left unread for days can lose a multiple of 2^32 on a busy counter.
//!
//! If the kernel starts withholding the statistics after the samplers
//! started (`net.inet.tcp.disable_access_to_stats` set while a process not
//! running as root reads them), every read is refused and the metrics stop
//! advancing. When reads resume, the totals add what the counters advanced
//! meanwhile, exactly if that is less than 2^32 - 2^28 and otherwise less a
//! multiple of 2^32.
//!
//! The first 38 fields of `struct tcpstat` (through `tcps_rcvoobyte`, 152
//! bytes) are read. The first 50 fields have the same order in every XNU
//! release from xnu-6153 (macOS 10.15) through xnu-12377 (macOS 26). Fields
//! after them have been removed and reordered between releases, which moves
//! `tcps_listendrop` and `tcps_pto`, so none of those is read.

mod mib;
mod retransmit;
mod traffic;

use crate::agent::sampler_status::Unsupported;

use std::sync::Mutex;
use std::time::Duration;

/// How often the widening thread reads the statistics while `tcp_traffic`
/// runs. A 32-bit byte counter takes about 250 ms to advance by 2^32 - 2^28
/// at 128 Gbit/s.
const WIDEN_INTERVAL: Duration = Duration::from_millis(250);

/// The leading fields of XNU's `struct tcpstat` (`bsd/netinet/tcp_var.h`),
/// through `tcps_rcvoobyte`, the last one a sampler reads.
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
    rcvduppack: u32,
    rcvdupbyte: u32,
    rcvpartduppack: u32,
    rcvpartdupbyte: u32,
    rcvoopack: u32,
    rcvoobyte: u32,
}

/// The 64-bit totals of the counters the samplers read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Totals {
    delack: u64,
    rexmttimeo: u64,
    sndbyte: u64,
    sndrexmitpack: u64,
    rcvbyte: u64,
    rcvbadsum: u64,
    rcvbadoff: u64,
    rcvshort: u64,
    rcvoobyte: u64,
}

/// One `Wide` per counter in `Totals`.
#[derive(Default)]
struct Counters {
    delack: Wide,
    rexmttimeo: Wide,
    sndbyte: Wide,
    sndrexmitpack: Wide,
    rcvbyte: Wide,
    rcvbadsum: Wide,
    rcvbadoff: Wide,
    rcvshort: Wide,
    rcvoobyte: Wide,
}

impl Counters {
    const fn new() -> Self {
        Self {
            delack: Wide::new(),
            rexmttimeo: Wide::new(),
            sndbyte: Wide::new(),
            sndrexmitpack: Wide::new(),
            rcvbyte: Wide::new(),
            rcvbadsum: Wide::new(),
            rcvbadoff: Wide::new(),
            rcvshort: Wide::new(),
            rcvoobyte: Wide::new(),
        }
    }

    fn update(&mut self, s: &TcpStat) -> Totals {
        Totals {
            delack: self.delack.update(s.delack),
            rexmttimeo: self.rexmttimeo.update(s.rexmttimeo),
            sndbyte: self.sndbyte.update(s.sndbyte),
            sndrexmitpack: self.sndrexmitpack.update(s.sndrexmitpack),
            rcvbyte: self.rcvbyte.update(s.rcvbyte),
            rcvbadsum: self.rcvbadsum.update(s.rcvbadsum),
            rcvbadoff: self.rcvbadoff.update(s.rcvbadoff),
            rcvshort: self.rcvshort.update(s.rcvshort),
            rcvoobyte: self.rcvoobyte.update(s.rcvoobyte),
        }
    }
}

static COUNTERS: Mutex<Counters> = Mutex::new(Counters::new());

/// Read the statistics and add them to `COUNTERS`, or `None` if `read`
/// refuses the read.
fn sample() -> Option<Totals> {
    sample_into(&COUNTERS, read)
}

/// Call `read` and add its result to `counters`, with the lock held across
/// both, so the totals advance in the order the reads were taken.
fn sample_into(
    counters: &Mutex<Counters>,
    read: impl FnOnce() -> Option<TcpStat>,
) -> Option<Totals> {
    let mut counters = counters.lock().unwrap();
    let s = read()?;
    Some(counters.update(&s))
}

/// Start the widening thread, once per process.
fn start_widening() -> std::io::Result<()> {
    static STARTED: Mutex<bool> = Mutex::new(false);

    let mut started = STARTED.lock().unwrap();
    if !*started {
        std::thread::Builder::new()
            .name("tcp-widen".to_string())
            .spawn(|| loop {
                std::thread::sleep(WIDEN_INTERVAL);
                let _ = sample();
            })?;
        *started = true;
    }
    Ok(())
}

/// One read of the TCP statistics, or `None` if the read fails or counts no
/// packet sent or received. The kernel returns zeros, successfully, when it
/// withholds the statistics, which happens if
/// `net.inet.tcp.disable_access_to_stats` is set while a process not running
/// as root reads them; taking those zeros as counts would make `Wide` read
/// the fall to zero as a wrap: a metric whose counter is between 2^28 and
/// 2^32 - 2^28 would gain 2^32 across the zero read and the next read, and
/// one whose counter is higher would jump by 2^32 less the counter and hold
/// until the counter wraps.
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

/// The largest fall between two reads taken as a racing update rather than a
/// wrap. A lost update is what other CPUs added to the counter between one
/// thread's load and store: usually a few segments, more if the thread is
/// preempted between the two. A fall of `MAX_BACKWARD` or more is taken as a
/// wrap and adds about 2^32.
const MAX_BACKWARD: u32 = 1 << 28;

/// A 32-bit kernel counter as a 64-bit total: zero at the first read, then
/// each read adds the increase since the previous one, modulo 2^32. A fall of
/// less than `MAX_BACKWARD` is taken as a read lower than the previous one
/// (see the module doc): it adds nothing, and the next increase is taken from
/// the higher value.
#[derive(Default)]
struct Wide {
    last: Option<u32>,
    total: u64,
}

impl Wide {
    const fn new() -> Self {
        Self {
            last: None,
            total: 0,
        }
    }

    fn update(&mut self, value: u32) -> u64 {
        match self.last {
            None => self.last = Some(value),
            Some(last) => {
                let fall = last.wrapping_sub(value);
                if fall == 0 || fall >= MAX_BACKWARD {
                    self.total += value.wrapping_sub(last) as u64;
                    self.last = Some(value);
                }
            }
        }
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Totals in which each counter has a different power of two, through
    /// `Counters::update`, so a metric summed or read from the wrong counter
    /// reads a different value.
    pub(super) fn distinct_totals() -> Totals {
        let mut counters = Counters::default();
        let mut s = TcpStat::default();
        counters.update(&s);

        s.delack = 1;
        s.rexmttimeo = 2;
        s.sndbyte = 4;
        s.sndrexmitpack = 8;
        s.rcvbyte = 16;
        s.rcvbadsum = 32;
        s.rcvbadoff = 64;
        s.rcvshort = 128;
        s.rcvoobyte = 256;
        // Fields no metric reads.
        s.sndtotal = 512;
        s.rcvtotal = 1024;
        s.sndrexmitbyte = 2048;
        s.rcvoopack = 4096;
        s.rcvdupbyte = 8192;
        counters.update(&s)
    }

    #[test]
    fn wide_ignores_a_read_lower_than_the_previous_one() {
        let mut c = Wide::default();
        assert_eq!(c.update(1_000), 0);
        assert_eq!(c.update(1_500), 500);
        assert_eq!(c.update(1_400), 500);
        assert_eq!(c.update(1_600), 600);
    }

    // An advance short of 2^32 - 2^28 is counted whole, with or without a
    // wrap, and counting continues from it.
    #[test]
    fn wide_counts_a_large_advance() {
        let mut c = Wide::default();
        assert_eq!(c.update(0), 0);
        assert_eq!(c.update(3_000_000_000), 3_000_000_000);
        assert_eq!(c.update(5), 3_000_000_000 + (1 << 32) - 3_000_000_000 + 5);
        assert_eq!(c.update(10), (1 << 32) + 10);
    }

    // A fall just short of `MAX_BACKWARD` is a racing update; a fall of
    // `MAX_BACKWARD` is a wrap, and an advance of 2^32 - 2^28 is counted
    // whole.
    #[test]
    fn wide_takes_max_backward_as_the_boundary() {
        let mut c = Wide::default();
        c.update(MAX_BACKWARD);
        assert_eq!(c.update(1), 0);

        let mut c = Wide::default();
        c.update(MAX_BACKWARD);
        assert_eq!(c.update(0), (1 << 32) - MAX_BACKWARD as u64);

        let mut c = Wide::default();
        c.update(0);
        assert_eq!(c.update(u32::MAX - MAX_BACKWARD + 1), (1 << 32) - (1 << 28));
    }

    // A reader that finds the lock free was called without it held. This
    // does not catch a lock released and taken again between the read and
    // the update.
    #[test]
    fn sample_reads_with_the_lock_held() {
        let counters = Mutex::new(Counters::new());
        let totals = sample_into(&counters, || {
            assert!(counters.try_lock().is_err());
            Some(TcpStat {
                sndtotal: 1,
                ..TcpStat::default()
            })
        });
        assert!(totals.is_some());
    }

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
        assert_eq!(offset_of!(TcpStat, rcvoobyte), 148);
        assert_eq!(std::mem::size_of::<TcpStat>(), 152);
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
