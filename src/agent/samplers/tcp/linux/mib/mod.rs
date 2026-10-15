//! TCP counters from the kernel's SNMP MIBs: delayed ACKs, retransmission
//! timeouts and loss recovery, zero-window advertisements and drops,
//! receive-memory pruning and drops, listen-queue overflows, aborts, and the
//! `Tcp` MIB group's failures, resets, retransmitted segments and
//! established-connection count. One read and parse of each of
//! `/proc/net/netstat` (its `TcpExt` section) and `/proc/net/snmp` (its `Tcp`
//! section) per refresh.
//!
//! Why procfs (principle 15): the kernel maintains these as per-CPU SNMP MIB
//! counters, incremented with `NET_INC_STATS`/`TCP_INC_STATS` at each event
//! site across the TCP stack, and exposes them only through those two files.
//! Covering them with BPF would need one hook per event type, some of them
//! inside static functions the compiler may inline, and would repeat counting
//! the kernel already does on every one of those paths. The parse is
//! O(columns) and reuses one buffer. The kernel sums each counter over every
//! possible CPU when the file is read, so the read cost grows with CPU count;
//! it has not been measured at fleet scale (principle 16).
//!
//! Each file prints a section as two lines, `Prefix: Name1 Name2 ...` then
//! `Prefix: v1 v2 ...`. Which columns exist, and in what order, varies by
//! kernel version, so values are matched to names by position within the
//! pair, never by a fixed column index. A name the running kernel does not
//! print leaves its metric never set, which the snapshot reports as absent
//! rather than 0. A section whose header and value lines differ in length is
//! not read at all: its metrics keep their previous values and its window is
//! not advanced.
//!
//! The counters cover the network namespace the agent runs in, and count IPv4
//! and IPv6 connections together (both families increment the same MIB).
//!
//! Metric names carry the protocol and what is counted, not the file or the
//! kernel's column name; each description names the column it comes from.

const NAME: &str = "tcp_mib";

use crate::agent::timing::AcquisitionGroup;
use crate::agent::*;

use metriken::{LazyCounter, LazyGauge};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use std::collections::HashMap;

mod stats;

use stats::*;

/// `/proc/net/netstat`, `TcpExt` section: one column to one counter.
const NETSTAT_COUNTERS: &[(&str, &LazyCounter)] = &[
    // ACKs
    ("DelayedACKs", &TCP_DELAYED_ACKS),
    ("DelayedACKLocked", &TCP_DELAYED_ACK_DEFERRALS),
    ("DelayedACKLost", &TCP_DUPLICATE_DATA_SEGMENTS),
    // retransmission and loss recovery
    ("TCPTimeouts", &TCP_RTO_EXPIRATIONS),
    ("TCPLossProbes", &TCP_LOSS_PROBES),
    ("TCPLossProbeRecovery", &TCP_LOSS_PROBE_RECOVERIES),
    ("TCPFastRetrans", &TCP_LOSS_RETRANSMITS_FAST),
    ("TCPSlowStartRetrans", &TCP_LOSS_RETRANSMITS_SLOW_START),
    ("TCPLostRetransmit", &TCP_RETRANSMITS_LOST),
    ("TCPSpuriousRTOs", &TCP_SPURIOUS_RTOS),
    ("TCPSackRecovery", &TCP_FAST_RECOVERIES_SACK),
    ("TCPRenoRecovery", &TCP_FAST_RECOVERIES_RENO),
    ("TCPDSACKRecv", &TCP_DSACKS_RECEIVED_OLD),
    ("TCPDSACKOfoRecv", &TCP_DSACKS_RECEIVED_OUT_OF_ORDER),
    ("TCPDSACKOldSent", &TCP_DSACKS_SENT_OLD),
    ("TCPDSACKOfoSent", &TCP_DSACKS_SENT_OUT_OF_ORDER),
    // receive window
    ("TCPToZeroWindowAdv", &TCP_ZERO_WINDOW_CLOSED),
    ("TCPFromZeroWindowAdv", &TCP_ZERO_WINDOW_REOPENED),
    ("TCPWantZeroWindowAdv", &TCP_ZERO_WINDOW_WITHHELD),
    ("TCPZeroWindowDrop", &TCP_ZERO_WINDOW_DROPS),
    // receive memory
    ("PruneCalled", &TCP_PRUNE_ATTEMPTS),
    ("RcvPruned", &TCP_PRUNE_FAILURES),
    ("OfoPruned", &TCP_OUT_OF_ORDER_PRUNES),
    ("TCPRcvCollapsed", &TCP_COLLAPSED_BUFFERS),
    ("TCPBacklogDrop", &TCP_BACKLOG_DROPS),
    ("TCPRcvQDrop", &TCP_RCVBUF_DROPS_IN_ORDER),
    ("TCPOFODrop", &TCP_RCVBUF_DROPS_OUT_OF_ORDER),
    ("TCPOFOQueue", &TCP_OUT_OF_ORDER_QUEUED),
    ("TCPMemoryPressures", &TCP_MEMORY_PRESSURE_ENTRIES),
    // send path
    ("TCPAutoCorking", &TCP_AUTOCORKS),
    // listeners
    ("ListenOverflows", &TCP_LISTEN_OVERFLOWS),
    ("ListenDrops", &TCP_LISTEN_DROPS),
    // aborts
    ("TCPAbortOnData", &TCP_ABORTS_DATA),
    ("TCPAbortOnTimeout", &TCP_ABORTS_TIMEOUT),
    ("TCPAbortOnMemory", &TCP_ABORTS_MEMORY),
    ("TCPAbortOnClose", &TCP_ABORTS_CLOSE),
    ("TCPAbortOnLinger", &TCP_ABORTS_LINGER),
];

/// `/proc/net/snmp`, `Tcp` section: one column to one counter.
const SNMP_COUNTERS: &[(&str, &LazyCounter)] = &[
    ("AttemptFails", &TCP_OPEN_FAILURES),
    ("EstabResets", &TCP_ESTABLISHED_RESETS),
    ("InErrs", &TCP_INPUT_ERRORS),
    ("InCsumErrors", &TCP_CHECKSUM_ERRORS),
    ("OutRsts", &TCP_RESETS_SENT),
    ("RetransSegs", &TCP_SEGMENTS_RETRANSMITTED),
];

/// `/proc/net/snmp`, `Tcp` section: one column to one gauge.
const SNMP_GAUGES: &[(&str, &LazyGauge)] = &[("CurrEstab", &TCP_ESTABLISHED_CONNECTIONS)];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = TcpMibInner::new()?;

    Ok(Some(Box::new(TcpMib {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct TcpMib {
    inner: Mutex<TcpMibInner>,
}

#[async_trait]
impl Sampler for TcpMib {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        inner.refresh().await;
    }
}

struct TcpMibInner {
    data: String,
    netstat: Source,
    snmp: Source,
}

impl TcpMibInner {
    fn new() -> Result<Self, std::io::Error> {
        Ok(Self {
            data: String::new(),
            netstat: Source::open(
                "/proc/net/netstat",
                Section::new("TcpExt", NETSTAT_COUNTERS, &[]),
                &NETSTAT_ACQ,
            )?,
            snmp: Source::open(
                "/proc/net/snmp",
                Section::new("Tcp", SNMP_COUNTERS, SNMP_GAUGES),
                &SNMP_ACQ,
            )?,
        })
    }

    /// The two files are independent read sections: a failure of one does
    /// not skip the other.
    async fn refresh(&mut self) {
        let _ = self.netstat.refresh(&mut self.data).await;
        let _ = self.snmp.refresh(&mut self.data).await;
    }
}

/// One procfs file, the section read from it, and that read's acquisition
/// group.
struct Source {
    path: &'static str,
    file: File,
    section: Section,
    acq: &'static AcquisitionGroup,
    /// A malformed section is reported once, not on every refresh.
    warned: bool,
}

impl Source {
    fn open(
        path: &'static str,
        section: Section,
        acq: &'static AcquisitionGroup,
    ) -> Result<Self, std::io::Error> {
        let file = std::fs::File::open(path).map(File::from_std)?;

        Ok(Self {
            path,
            file,
            section,
            acq,
            warned: false,
        })
    }

    // Acquisition-group bracket (principle 18): acquire before the read,
    // finish after every value from the parse is set. A read error (`?`)
    // drops the guard without `finish()`, and so does a section that is
    // missing or whose header and value lines disagree: no value was set, and
    // the previous window stands (missing beats wrong).
    async fn refresh(&mut self, data: &mut String) -> Result<(), std::io::Error> {
        let guard = self.acq.acquire();

        self.file.rewind().await?;

        data.clear();

        self.file.read_to_string(data).await?;

        match parse(data, &self.section) {
            Ok(()) => guard.finish(),
            Err(e) => {
                if !self.warned {
                    warn!(
                        "{NAME}: {}: {} section not read: {e}",
                        self.path, self.section.prefix
                    );
                    self.warned = true;
                }
                guard.discard();
            }
        }

        Ok(())
    }
}

/// One section's lookup tables, built once.
struct Section {
    prefix: &'static str,
    counters: HashMap<&'static str, &'static LazyCounter>,
    gauges: HashMap<&'static str, &'static LazyGauge>,
}

impl Section {
    fn new(
        prefix: &'static str,
        counters: &'static [(&'static str, &'static LazyCounter)],
        gauges: &'static [(&'static str, &'static LazyGauge)],
    ) -> Self {
        Self {
            prefix,
            counters: counters.iter().copied().collect(),
            gauges: gauges.iter().copied().collect(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ParseError {
    /// The file has no header and value line pair for the section.
    Missing,
    /// The header and value lines have different numbers of columns, so no
    /// value can be matched to its name with confidence.
    Mismatch { names: usize, values: usize },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no header and value lines"),
            Self::Mismatch { names, values } => {
                write!(f, "{names} column names but {values} values")
            }
        }
    }
}

/// Set every metric whose column appears in `section`'s pair of lines in
/// `data`. Columns the text lacks, and values that do not parse as an
/// unsigned 64-bit integer, leave their metric untouched. A missing section or a
/// header/value length mismatch sets nothing and returns an error.
fn parse(data: &str, section: &Section) -> Result<(), ParseError> {
    let mut lines = data.lines().filter_map(|line| {
        let (prefix, rest) = line.split_once(':')?;
        (prefix == section.prefix).then_some(rest)
    });

    let (Some(header), Some(values)) = (lines.next(), lines.next()) else {
        return Err(ParseError::Missing);
    };

    let names = header.split_whitespace().count();
    let count = values.split_whitespace().count();
    if names != count {
        return Err(ParseError::Mismatch {
            names,
            values: count,
        });
    }

    for (name, value) in header.split_whitespace().zip(values.split_whitespace()) {
        if let Some(counter) = section.counters.get(name) {
            if let Ok(v) = value.parse::<u64>() {
                counter.set(v);
            }
        } else if let Some(gauge) = section.gauges.get(name) {
            // The kernel prints these with %lu: a sum over per-CPU
            // counters that can be transiently negative (CurrEstab read
            // while one CPU has counted an increment and another the
            // matching decrement) prints as a value near 2^64. Reading it
            // back as two's complement gives that negative value; leaving
            // the gauge unset would pair the previous value with this
            // read's window.
            if let Ok(v) = value.parse::<u64>() {
                gauge.set(v as i64);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use metriken::{Counter, Gauge};

    /// `/proc/net/netstat` as a 6.12 kernel prints it: the `TcpExt` column
    /// list of `snmp4_net_list` in net/ipv4/proc.c, then `IpExt` and a
    /// shortened `MPTcpExt`. Every mapped column has its own value; the rest
    /// are filler.
    const NETSTAT: &str = "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled RcvPruned OfoPruned OutOfWindowIcmps LockDroppedIcmps ArpFilter TW TWRecycled TWKilled PAWSActive PAWSEstab DelayedACKs DelayedACKLocked DelayedACKLost ListenOverflows ListenDrops TCPHPHits TCPPureAcks TCPHPAcks TCPRenoRecovery TCPSackRecovery TCPSACKReneging TCPSACKReorder TCPRenoReorder TCPTSReorder TCPFullUndo TCPPartialUndo TCPDSACKUndo TCPLossUndo TCPLostRetransmit TCPRenoFailures TCPSackFailures TCPLossFailures TCPFastRetrans TCPSlowStartRetrans TCPTimeouts TCPLossProbes TCPLossProbeRecovery TCPRenoRecoveryFail TCPSackRecoveryFail TCPRcvCollapsed TCPBacklogCoalesce TCPDSACKOldSent TCPDSACKOfoSent TCPDSACKRecv TCPDSACKOfoRecv TCPAbortOnData TCPAbortOnClose TCPAbortOnMemory TCPAbortOnTimeout TCPAbortOnLinger TCPAbortFailed TCPMemoryPressures TCPMemoryPressuresChrono TCPSACKDiscard TCPDSACKIgnoredOld TCPDSACKIgnoredNoUndo TCPSpuriousRTOs TCPMD5NotFound TCPMD5Unexpected TCPMD5Failure TCPSackShifted TCPSackMerged TCPSackShiftFallback TCPBacklogDrop PFMemallocDrop TCPMinTTLDrop TCPDeferAcceptDrop IPReversePathFilter TCPTimeWaitOverflow TCPReqQFullDoCookies TCPReqQFullDrop TCPRetransFail TCPRcvCoalesce TCPOFOQueue TCPOFODrop TCPOFOMerge TCPChallengeACK TCPSYNChallenge TCPFastOpenActive TCPFastOpenActiveFail TCPFastOpenPassive TCPFastOpenPassiveFail TCPFastOpenListenOverflow TCPFastOpenCookieReqd TCPFastOpenBlackhole TCPSpuriousRtxHostQueues BusyPollRxPackets TCPAutoCorking TCPFromZeroWindowAdv TCPToZeroWindowAdv TCPWantZeroWindowAdv TCPSynRetrans TCPOrigDataSent TCPHystartTrainDetect TCPHystartTrainCwnd TCPHystartDelayDetect TCPHystartDelayCwnd TCPACKSkippedSynRecv TCPACKSkippedPAWS TCPACKSkippedSeq TCPACKSkippedFinWait2 TCPACKSkippedTimeWait TCPACKSkippedChallenge TCPWinProbe TCPKeepAlive TCPMTUPFail TCPMTUPSuccess TCPDelivered TCPDeliveredCE TCPAckCompressed TCPZeroWindowDrop TCPRcvQDrop TCPWqueueTooBig TCPFastOpenPassiveAltKey TcpTimeoutRehash TcpDuplicateDataRehash TCPDSACKRecvSegs TCPDSACKIgnoredDubious TCPMigrateReqSuccess TCPMigrateReqFailure TCPPLBRehash TCPAORequired TCPAOBad TCPAOKeyNotFound TCPAOGood TCPAODroppedIcmps
TcpExt: 0 7922 15841 23760 388 2 41 55436 63355 71274 79193 87112 95031 2950 10869 4818263 1290 20417 1203 1210 58383 66302 74221 3 12006 97978 5897 13816 21735 29654 37573 45492 53411 2071 69249 77168 85087 45512 311 8803 66120 1734 32601 40520 9150 56358 20188 133 10032 64 5604 2118 1 88 7 35548 4 51386 59305 67224 75143 97 90981 98900 6819 14738 22657 30576 6 46414 54333 62252 70171 78090 86009 93928 1847 9766 230981 15 33523 41442 49361 57280 65199 73118 81037 88956 96875 4794 12713 20632 771204 417 418 5521 60227 68146 76065 83984 91903 99822 7741 15660 23579 31498 39417 47336 55255 63174 71093 79012 86931 94850 2769 12 9 26526 34445 42364 50283 58202 66121 74040 81959 89878 97797 5716 13635 21554 29473
IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets OutOctets InMcastOctets OutMcastOctets InBcastOctets OutBcastOctets InCsumErrors InNoECTPkts InECT1Pkts InECT0Pkts InCEPkts ReasmOverlaps
IpExt: 5 7924 15843 23762 31681 39600 47519 55438 63357 71276 79195 87114 95033 2952 10871 18790 26709 34628
MPTcpExt: MPCapableSYNRX MPCapableSYNTX MPCapableSYNACKRX MPCapableACKRX MPCapableFallbackACK MPCapableFallbackSYNACK
MPTcpExt: 0 0 0 0 0 0
";

    /// `/proc/net/snmp` as a 6.12 kernel prints it. `Ip`, `Icmp`, `IpExt` (in
    /// the other file) and `Udp` also have an `InCsumErrors` column, and `Tcp`
    /// has a signed `MaxConn` of -1, so the section match and the per-type
    /// value parse are both exercised.
    const SNMP: &str = "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates OutTransmits
Ip: 1 64 15849 23768 31687 39606 47525 55444 63363 71282 79201 87120 95039 2958 10877 18796 26715 34634 42553 50472
Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs OutErrors OutRateLimitGlobal OutRateLimitHost OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps
Icmp: 7 7926 15845 23764 31683 39602 47521 55440 63359 71278 79197 87116 95035 2954 10873 18792 26711 34630 42549 50468 58387 66306 74225 82144 90063 97982 5901 13820 21739
IcmpMsg: InType0 InType3 InType8 OutType0 OutType3 OutType8
IcmpMsg: 12 3301 70 70 2950 12
Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors
Tcp: 1 200 120000 -1 402117 388190 30872 14421 1873 912884711 1002318842 129033 57 61770 5
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors
Udp: 13 7932 15851 23770 31689 39608 47527 55446 63365
UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors
UdpLite: 0 0 0 0 0 0 0 0 0
";

    #[test]
    fn netstat_tcpext_parses() {
        let section = Section::new("TcpExt", NETSTAT_COUNTERS, &[]);
        assert_eq!(parse(NETSTAT, &section), Ok(()));
        assert_eq!(TCP_DELAYED_ACKS.value(), 4818263);
        assert_eq!(TCP_DELAYED_ACK_DEFERRALS.value(), 1290);
        assert_eq!(TCP_DUPLICATE_DATA_SEGMENTS.value(), 20417);
        assert_eq!(TCP_RTO_EXPIRATIONS.value(), 8803);
        assert_eq!(TCP_LOSS_PROBES.value(), 66120);
        assert_eq!(TCP_LOSS_PROBE_RECOVERIES.value(), 1734);
        assert_eq!(TCP_LOSS_RETRANSMITS_FAST.value(), 45512);
        assert_eq!(TCP_LOSS_RETRANSMITS_SLOW_START.value(), 311);
        assert_eq!(TCP_RETRANSMITS_LOST.value(), 2071);
        assert_eq!(TCP_SPURIOUS_RTOS.value(), 97);
        assert_eq!(TCP_FAST_RECOVERIES_SACK.value(), 12006);
        assert_eq!(TCP_FAST_RECOVERIES_RENO.value(), 3);
        assert_eq!(TCP_DSACKS_RECEIVED_OLD.value(), 10032);
        assert_eq!(TCP_DSACKS_RECEIVED_OUT_OF_ORDER.value(), 64);
        assert_eq!(TCP_DSACKS_SENT_OLD.value(), 20188);
        assert_eq!(TCP_DSACKS_SENT_OUT_OF_ORDER.value(), 133);
        assert_eq!(TCP_ZERO_WINDOW_CLOSED.value(), 418);
        assert_eq!(TCP_ZERO_WINDOW_REOPENED.value(), 417);
        assert_eq!(TCP_ZERO_WINDOW_WITHHELD.value(), 5521);
        assert_eq!(TCP_ZERO_WINDOW_DROPS.value(), 12);
        assert_eq!(TCP_PRUNE_ATTEMPTS.value(), 388);
        assert_eq!(TCP_PRUNE_FAILURES.value(), 2);
        assert_eq!(TCP_OUT_OF_ORDER_PRUNES.value(), 41);
        assert_eq!(TCP_COLLAPSED_BUFFERS.value(), 9150);
        assert_eq!(TCP_BACKLOG_DROPS.value(), 6);
        assert_eq!(TCP_RCVBUF_DROPS_IN_ORDER.value(), 9);
        assert_eq!(TCP_RCVBUF_DROPS_OUT_OF_ORDER.value(), 15);
        assert_eq!(TCP_OUT_OF_ORDER_QUEUED.value(), 230981);
        assert_eq!(TCP_MEMORY_PRESSURE_ENTRIES.value(), 4);
        assert_eq!(TCP_AUTOCORKS.value(), 771204);
        assert_eq!(TCP_LISTEN_OVERFLOWS.value(), 1203);
        assert_eq!(TCP_LISTEN_DROPS.value(), 1210);
        assert_eq!(TCP_ABORTS_DATA.value(), 5604);
        assert_eq!(TCP_ABORTS_TIMEOUT.value(), 88);
        assert_eq!(TCP_ABORTS_MEMORY.value(), 1);
        assert_eq!(TCP_ABORTS_CLOSE.value(), 2118);
        assert_eq!(TCP_ABORTS_LINGER.value(), 7);
    }

    #[test]
    fn snmp_tcp_parses() {
        let section = Section::new("Tcp", SNMP_COUNTERS, SNMP_GAUGES);
        assert_eq!(parse(SNMP, &section), Ok(()));
        assert_eq!(TCP_OPEN_FAILURES.value(), 30872);
        assert_eq!(TCP_ESTABLISHED_RESETS.value(), 14421);
        assert_eq!(TCP_INPUT_ERRORS.value(), 57);
        // Tcp's InCsumErrors, not Ip's, Icmp's or Udp's
        assert_eq!(TCP_CHECKSUM_ERRORS.value(), 5);
        assert_eq!(TCP_RESETS_SENT.value(), 61770);
        assert_eq!(TCP_SEGMENTS_RETRANSMITTED.value(), 129033);
        assert_eq!(TCP_ESTABLISHED_CONNECTIONS.value(), 1873);
    }

    /// The column names of `prefix`'s header line in `data`.
    fn header<'a>(data: &'a str, prefix: &str) -> Vec<&'a str> {
        data.lines()
            .find_map(|l| {
                let (p, rest) = l.split_once(':')?;
                (p == prefix).then_some(rest)
            })
            .unwrap_or_else(|| panic!("no {prefix} section"))
            .split_whitespace()
            .collect()
    }

    /// Every mapped column is one the kernel prints, exactly once, in the
    /// section it is read from; no column is mapped twice; and no metric is
    /// the target of two columns. A typo would leave a metric unset forever,
    /// and a duplicate would make two metrics of one column or one metric of
    /// two.
    #[test]
    fn every_key_is_in_its_section_exactly_once() {
        let sections: [(&str, &str, Vec<&str>); 2] = [
            (
                NETSTAT,
                "TcpExt",
                NETSTAT_COUNTERS.iter().map(|(k, _)| *k).collect(),
            ),
            (
                SNMP,
                "Tcp",
                SNMP_COUNTERS
                    .iter()
                    .map(|(k, _)| *k)
                    .chain(SNMP_GAUGES.iter().map(|(k, _)| *k))
                    .collect(),
            ),
        ];
        for (data, prefix, keys) in sections {
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                keys.len(),
                "a {prefix} column is mapped twice"
            );

            let columns = header(data, prefix);
            for key in keys {
                let n = columns.iter().filter(|c| **c == key).count();
                assert_eq!(n, 1, "{key} appears {n} times in the {prefix} header");
            }
        }

        let mut counters: Vec<*const LazyCounter> = NETSTAT_COUNTERS
            .iter()
            .chain(SNMP_COUNTERS)
            .map(|(_, c)| *c as *const LazyCounter)
            .collect();
        let total = counters.len();
        counters.sort_unstable();
        counters.dedup();
        assert_eq!(
            counters.len(),
            total,
            "a counter is mapped from two columns"
        );
    }

    // The tests below parse into metrics of their own, so they cannot race
    // with the two tests above, which write the sampler's real metrics.

    static A: LazyCounter = LazyCounter::new(Counter::default);
    static B: LazyCounter = LazyCounter::new(Counter::default);
    static G: LazyGauge = LazyGauge::new(Gauge::default);

    /// A column the text lacks leaves its metric untouched; the columns that
    /// are present are still set.
    #[test]
    fn missing_columns_leave_metrics_untouched() {
        static COUNTERS: &[(&str, &LazyCounter)] = &[("Alpha", &A), ("Beta", &B)];
        static GAUGES: &[(&str, &LazyGauge)] = &[("Gamma", &G)];
        let section = Section::new("Test", COUNTERS, GAUGES);
        B.set(41);
        G.set(-7);
        assert_eq!(parse("Test: Other Alpha\nTest: 9 1\n", &section), Ok(()));
        assert_eq!(A.value(), 1);
        assert_eq!(B.value(), 41);
        assert_eq!(G.value(), -7);
    }

    static C: LazyCounter = LazyCounter::new(Counter::default);
    static D: LazyCounter = LazyCounter::new(Counter::default);

    /// A header and value line of different lengths is refused whole, in
    /// both directions, without panicking: a positional match could put any
    /// value on the wrong metric.
    #[test]
    fn column_count_mismatch_sets_nothing() {
        static COUNTERS: &[(&str, &LazyCounter)] = &[("Gamma", &C), ("Delta", &D)];
        let section = Section::new("Test", COUNTERS, &[]);
        C.set(5);
        D.set(6);
        assert_eq!(
            parse("Test: Gamma Delta\nTest: 1\n", &section),
            Err(ParseError::Mismatch {
                names: 2,
                values: 1
            })
        );
        assert_eq!(
            parse("Test: Gamma Delta\nTest: 1 2 3\n", &section),
            Err(ParseError::Mismatch {
                names: 2,
                values: 3
            })
        );
        assert_eq!(C.value(), 5);
        assert_eq!(D.value(), 6);
    }

    static E: LazyCounter = LazyCounter::new(Counter::default);

    /// A section with no value line, a file without the section, and a
    /// section whose prefix only begins with the wanted one are all
    /// `Missing`; `Tcp` must not read `TcpExt`.
    #[test]
    fn missing_section_sets_nothing() {
        static COUNTERS: &[(&str, &LazyCounter)] = &[("Epsilon", &E)];
        let section = Section::new("Tcp", COUNTERS, &[]);
        E.set(8);
        assert_eq!(parse("Tcp: Epsilon\n", &section), Err(ParseError::Missing));
        assert_eq!(
            parse("TcpExt: Epsilon\nTcpExt: 1\n", &section),
            Err(ParseError::Missing)
        );
        assert_eq!(parse("", &section), Err(ParseError::Missing));
        assert_eq!(E.value(), 8);
    }

    static F: LazyCounter = LazyCounter::new(Counter::default);
    static H: LazyGauge = LazyGauge::new(Gauge::default);

    /// A value that does not parse as an unsigned 64-bit integer leaves its
    /// metric untouched and does not stop the others.
    #[test]
    fn unparseable_value_leaves_its_metric_untouched() {
        static COUNTERS: &[(&str, &LazyCounter)] = &[("Zeta", &F), ("Theta", &T)];
        static GAUGES: &[(&str, &LazyGauge)] = &[("Eta", &H)];
        let section = Section::new("Test", COUNTERS, GAUGES);
        F.set(3);
        H.set(4);
        assert_eq!(
            parse(
                "Test: Zeta Eta Theta\nTest: -1 x 18446744073709551616\n",
                &section
            ),
            Ok(())
        );
        assert_eq!(F.value(), 3);
        assert_eq!(H.value(), 4);
        assert_eq!(T.value(), 0, "a value above u64::MAX is not wrapped");
    }

    static T: LazyCounter = LazyCounter::new(Counter::default);
    static W: LazyGauge = LazyGauge::new(Gauge::default);

    /// A gauge printed with %lu whose per-CPU sum is transiently negative
    /// (2^64 - 1 for -1) reads back as that negative value, so the read sets
    /// the gauge rather than leaving a stale value under a fresh window.
    #[test]
    fn wrapped_gauge_reads_as_negative() {
        static GAUGES: &[(&str, &LazyGauge)] = &[("CurrEstab", &W)];
        let section = Section::new("Tcp", &[], GAUGES);
        W.set(12);
        assert_eq!(
            parse("Tcp: CurrEstab\nTcp: 18446744073709551615\n", &section),
            Ok(())
        );
        assert_eq!(W.value(), -1);
        assert_eq!(parse("Tcp: CurrEstab\nTcp: 1873\n", &section), Ok(()));
        assert_eq!(W.value(), 1873);
    }

    static S: LazyCounter = LazyCounter::new(Counter::default);

    /// `Source::refresh` brackets each read with its acquisition group over a
    /// real file: a good read sets values and stamps a window, a section
    /// whose header and value lines disagree sets nothing and leaves the
    /// window where it was (warning once), a later good read stamps again,
    /// and a read error leaves the window unstamped.
    #[tokio::test]
    async fn source_refresh_stamps_only_a_good_read() {
        static COUNTERS: &[(&str, &LazyCounter)] = &[("X", &S)];
        static GOOD: AcquisitionGroup = AcquisitionGroup::new("tcp_mib", "test_good");
        static BAD: AcquisitionGroup = AcquisitionGroup::new("tcp_mib", "test_bad");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("netstat");
        std::fs::write(&path, "TcpExt: X\nTcpExt: 10\n").unwrap();
        let leaked: &'static str = Box::leak(path.to_str().unwrap().to_owned().into_boxed_str());
        let mut buf = String::new();

        let mut src = Source::open(leaked, Section::new("TcpExt", COUNTERS, &[]), &GOOD).unwrap();
        assert_eq!(GOOD.window(), None);
        src.refresh(&mut buf).await.unwrap();
        assert_eq!(S.value(), 10);
        let first = GOOD.window().expect("a good read stamps a window");

        // The file grew and moved the column: the read starts from the top.
        std::fs::write(&path, "TcpExt: Y X\nTcpExt: 0 11\n").unwrap();
        src.refresh(&mut buf).await.unwrap();
        assert_eq!(S.value(), 11);
        let second = GOOD.window().unwrap();
        assert_ne!(second, first, "a second good read stamps a new window");

        std::fs::write(&path, "TcpExt: X\nTcpExt: 12 13\n").unwrap();
        src.refresh(&mut buf).await.unwrap();
        src.refresh(&mut buf).await.unwrap();
        assert_eq!(S.value(), 11, "a mismatched section sets nothing");
        assert_eq!(GOOD.window(), Some(second), "and does not stamp");
        assert!(src.warned);

        std::fs::write(&path, "TcpExt: X\nTcpExt: 14\n").unwrap();
        src.refresh(&mut buf).await.unwrap();
        assert_eq!(S.value(), 14);
        let third = GOOD.window().unwrap();
        assert!(
            third.begin_ns > second.begin_ns,
            "a recovered read stamps again"
        );

        // A directory opens but cannot be read: the read error returns
        // before `finish()`.
        let dir_path: &'static str =
            Box::leak(dir.path().to_str().unwrap().to_owned().into_boxed_str());
        let mut src = Source::open(dir_path, Section::new("TcpExt", COUNTERS, &[]), &BAD).unwrap();
        assert!(src.refresh(&mut buf).await.is_err());
        assert_eq!(BAD.window(), None, "a failed read does not stamp");
        assert_eq!(S.value(), 14);
    }
}
