//! Network traffic on macOS: bytes and packets received and transmitted,
//! summed over interfaces, from each interface's counters, read with the
//! `IFMIB_IFDATA` sysctl (`net.link.generic.ifdata`).
//!
//! `network_bytes` and `network_packets` sum every interface, loopback,
//! bridges, tunnels and VM interfaces included, so a packet that crosses a
//! bridge and a member interface is counted on each, as on Linux.
//! `network_host_bytes` and `network_host_packets` sum the interfaces backed
//! by hardware: an `IONetworkInterface` in the I/O registry with no
//! `IOResources` ancestor (wifi, Ethernet on PCIe, Thunderbolt or USB, and
//! Thunderbolt networking), and the interfaces with no delegate interface
//! whose functional type is wifi or cellular (`awdl0`, Apple Wireless Direct
//! Link, used by AirDrop; `llw0`, low-latency WLAN; and `ap1`, all on the
//! wifi radio). They exclude `lo0`, bridges, `utun` tunnels (including one
//! delegated to a wifi interface) and the `vmenet` interfaces of virtual
//! machines (`IOUserEthernetInterface` under `IOResources`).
//!
//! `network_interface_errors` and `network_interface_drops` sum the input and
//! output errors (`ifi_ierrors`, `ifi_oerrors`), the input queue drops
//! (`ifi_iqdrops`) and the send queue drops (`ifmd_snd_drops`) of every
//! interface except those under `IOResources`. On macOS 26.6 a `vmenet`
//! interface's `ifi_iqdrops` grows in steps of about 0xAAAAAAAA that are not
//! drops, so the errors and drops of every interface under `IOResources` are
//! left out.
//!
//! An interface in neither the hardware-backed nor the `IOResources` set
//! when first listed (`lo0`, bridges, `utun`, or an interface `if_nameindex`
//! lists before the I/O registry does) is classified again on the first read
//! at least `SETTLE` later, and on each read after that until a
//! classification succeeds. Its errors and drops are held until then:
//! discarded if it is found under `IOResources` or goes away first, added to
//! the sums otherwise. One found hardware-backed then is in the host sums
//! from that read on.
//!
//! The counters start at zero when the sampler starts and add each
//! interface's increase since the previous read. An interface first listed
//! after the sampler started adds its whole count, since it was created after
//! the previous read; if it is in neither set, its errors and drops are held
//! as above. What an interface added stays in the sums after it goes away;
//! its traffic after the last read before it went away is not counted.
//!
//! The same counters from `NET_RT_IFLIST2`, the routing sysctl, give bytes
//! rounded down to 1 KiB and truncated to 32 bits to a process that is not an
//! Apple platform binary; this sysctl gives them whole.

const NAME: &str = "network_traffic";

use crate::agent::samplers::iokit::{CfString, Service};
use crate::agent::*;

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod stats;

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let Some(interfaces) = read_interfaces() else {
        return Err(
            crate::agent::sampler_status::Unsupported("if_nameindex failed".to_string()).into(),
        );
    };
    let Some(classes) = classify_interfaces() else {
        return Err(crate::agent::sampler_status::Unsupported(
            "classifying the interfaces failed (I/O registry, if_nameindex or socket)".to_string(),
        )
        .into());
    };

    let mut state = State::default();
    state.update(interfaces, Instant::now(), || Some(classes));

    Ok(Some(Box::new(Traffic {
        state: Mutex::new(state),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Traffic {
    state: Mutex<State>,
}

#[async_trait]
impl Sampler for Traffic {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut guard = COUNTERS_ACQ.acquire();

        let Some(interfaces) = read_interfaces() else {
            return;
        };
        // The window is the counter read; the registry walk in `update` is
        // not part of it.
        guard.mark_end();
        let mut state = self.state.lock().unwrap();
        state.update(interfaces, Instant::now(), classify_interfaces);

        let [rx_bytes, rx_packets, tx_bytes, tx_packets] = state.total;
        NETWORK_RX_BYTES.set(rx_bytes);
        NETWORK_RX_PACKETS.set(rx_packets);
        NETWORK_TX_BYTES.set(tx_bytes);
        NETWORK_TX_PACKETS.set(tx_packets);

        let [rx_bytes, rx_packets, tx_bytes, tx_packets] = state.host;
        NETWORK_RX_HOST_BYTES.set(rx_bytes);
        NETWORK_RX_HOST_PACKETS.set(rx_packets);
        NETWORK_TX_HOST_BYTES.set(tx_bytes);
        NETWORK_TX_HOST_PACKETS.set(tx_packets);

        let [rx_errors, tx_errors, rx_drops, tx_drops] = state.faults;
        NETWORK_RX_ERRORS.set(rx_errors);
        NETWORK_TX_ERRORS.set(tx_errors);
        NETWORK_RX_DROPS.set(rx_drops);
        NETWORK_TX_DROPS.set(tx_drops);

        guard.finish();
    }
}

/// Cumulative counters of one interface: received bytes, received packets,
/// transmitted bytes, transmitted packets, input errors, output errors, input
/// queue drops, send queue drops.
type Counters = [u64; 8];

/// Sums of four of the `Counters`: the first four, or the last four.
type Sums = [u64; 4];

/// The interfaces whose counters go into the sums other than
/// `network_bytes` and `network_packets`.
#[derive(Default)]
struct Classes {
    /// The interfaces `network_host_*` sums.
    host: HashSet<String>,
    /// The interfaces under `IOResources`, which errors and drops leave out.
    software: HashSet<String>,
}

/// One interface listed by `if_nameindex`, with its counters, or `None` if
/// they could not be read.
struct Reading {
    index: u32,
    name: String,
    counters: Option<Counters>,
}

/// The least time after an interface in neither of the `Classes` sets is
/// first listed before it is classified again, on the first read after that. The lag between `if_nameindex` and the I/O
/// registry listing a new `vmenet` has not been measured.
const SETTLE: Duration = Duration::from_secs(2);

/// Whether an interface's errors and drops go into the sums.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Faults {
    /// Not under `IOResources`: they are added.
    Counted,
    /// Under `IOResources`: they are left out.
    Excluded,
    /// In neither of the `Classes` sets when first listed, at `since`: the
    /// increases accumulate in `held`, which is added when it becomes
    /// `Counted` and dropped when it becomes `Excluded` or goes away.
    Unsettled { since: Instant, held: Sums },
}

/// An interface as of the last read.
struct Seen {
    name: String,
    host: bool,
    faults: Faults,
    last: Counters,
}

#[derive(Default)]
struct State {
    /// The interfaces of the last read, by index.
    seen: HashMap<u32, Seen>,
    /// Traffic summed over every interface and over hardware-backed
    /// interfaces.
    total: Sums,
    host: Sums,
    /// Errors and drops summed over every interface but those under
    /// `IOResources`.
    faults: Sums,
    /// False until the first read, whose counters are the baseline.
    started: bool,
    /// Interfaces listed but unreadable in every read since the first: their
    /// first readable counters are their baseline.
    baseline_pending: HashSet<(u32, String)>,
}

impl State {
    /// Add each interface's increase since the last read, taken at `at`.
    /// `classify` returns the interfaces' classes. It is called at most once,
    /// and only if a readable interface not seen before is listed, or a
    /// readable one whose `faults` has been `Unsettled` for at least
    /// `SETTLE`. If it returns `None`, an interface not seen before is left
    /// out of this read and is new again in the next; one already seen stays
    /// as it was.
    fn update(
        &mut self,
        readings: Vec<Reading>,
        at: Instant,
        classify: impl FnOnce() -> Option<Classes>,
    ) {
        let mut classify = Some(classify);
        let mut classes: Option<Option<Classes>> = None;
        let mut next = HashMap::with_capacity(readings.len());
        let mut pending = HashSet::new();

        for r in readings {
            let previous = self.seen.remove(&r.index).filter(|s| s.name == r.name);
            let key = (r.index, r.name);
            let in_baseline = !self.started || self.baseline_pending.contains(&key);
            let (index, name) = key;

            let Some(now) = r.counters else {
                // Listed but unreadable: keep the last counters, so the next
                // readable read adds the increase since then.
                if let Some(s) = previous {
                    next.insert(index, s);
                } else if in_baseline {
                    pending.insert((index, name));
                }
                continue;
            };

            let (is_host, mut faults, increase) = match previous {
                Some(s) => {
                    let mut faults = s.faults;
                    let mut is_host = s.host;
                    if let Faults::Unsettled { since, held } = faults {
                        if at.saturating_duration_since(since) >= SETTLE {
                            let found = classes
                                .get_or_insert_with(|| classify.take().and_then(|f| f()))
                                .as_ref();
                            if let Some(found) = found {
                                if found.software.contains(&name) {
                                    faults = Faults::Excluded;
                                } else {
                                    is_host = found.host.contains(&name);
                                    faults = Faults::Counted;
                                    add(&mut self.faults, &held);
                                }
                            }
                        }
                    }
                    (is_host, faults, increase(&now, &s.last))
                }
                None => {
                    let found = classes
                        .get_or_insert_with(|| classify.take().and_then(|f| f()))
                        .as_ref();
                    let Some(found) = found else {
                        if in_baseline {
                            pending.insert((index, name));
                        }
                        continue;
                    };
                    let increase = if in_baseline { [0; 8] } else { now };
                    let is_host = found.host.contains(&name);
                    let faults = if found.software.contains(&name) {
                        Faults::Excluded
                    } else if is_host {
                        Faults::Counted
                    } else {
                        Faults::Unsettled {
                            since: at,
                            held: [0; 4],
                        }
                    };
                    (is_host, faults, increase)
                }
            };

            let (traffic, fault_increase) = increase.split_at(4);
            add(&mut self.total, traffic);
            if is_host {
                add(&mut self.host, traffic);
            }
            match &mut faults {
                Faults::Counted => add(&mut self.faults, fault_increase),
                Faults::Excluded => {}
                Faults::Unsettled { held, .. } => add(held, fault_increase),
            }
            next.insert(
                index,
                Seen {
                    name,
                    host: is_host,
                    faults,
                    last: now,
                },
            );
        }

        self.seen = next;
        self.baseline_pending = pending;
        self.started = true;
    }
}

/// The increase from `last` to `now`. A counter lower than its last value
/// was reset, so all of its current value is new.
fn increase(now: &Counters, last: &Counters) -> Counters {
    std::array::from_fn(|i| now[i].checked_sub(last[i]).unwrap_or(now[i]))
}

fn add(sum: &mut Sums, increase: &[u64]) {
    for (s, i) in sum.iter_mut().zip(increase) {
        *s = s.wrapping_add(*i);
    }
}

/// Every interface `if_nameindex` lists, with its counters, or `None` if
/// `if_nameindex` fails.
fn read_interfaces() -> Option<Vec<Reading>> {
    Some(
        interface_names()?
            .into_iter()
            .map(|(index, name)| Reading {
                index,
                counters: counters(index, &name),
                name: name.to_string_lossy().into_owned(),
            })
            .collect(),
    )
}

/// The index and name of every interface `if_nameindex` lists, or `None` if
/// it fails.
fn interface_names() -> Option<Vec<(u32, CString)>> {
    // SAFETY: no preconditions; returns a list to free with
    // `if_freenameindex`, or null.
    let list = unsafe { libc::if_nameindex() };
    if list.is_null() {
        return None;
    }

    let mut out = Vec::new();
    let mut p = list;
    loop {
        // SAFETY: `p` points into the array `if_nameindex` returned, which
        // ends with an entry whose index is 0.
        let entry = unsafe { &*p };
        if entry.if_index == 0 {
            break;
        }
        // SAFETY: a listed entry's name is a NUL-terminated string.
        let name = unsafe { CStr::from_ptr(entry.if_name) };
        out.push((entry.if_index, name.to_owned()));
        // SAFETY: the entry at `p` was not the terminator, so `p + 1` is
        // within the array.
        p = unsafe { p.add(1) };
    }

    // SAFETY: `list` came from `if_nameindex` and is freed once.
    unsafe { libc::if_freenameindex(list) };
    Some(out)
}

/// The counters of interface `index`, or `None` if the read fails or the
/// index now names another interface. `ifmd_snd_drops` is 32 bits; when it
/// wraps, `increase` takes it for a reset and adds only its new value.
fn counters(index: u32, name: &CStr) -> Option<Counters> {
    let m = ifmib(index, name)?;
    let d = m.ifmd_data;
    Some([
        d.ifi_ibytes,
        d.ifi_ipackets,
        d.ifi_obytes,
        d.ifi_opackets,
        d.ifi_ierrors,
        d.ifi_oerrors,
        d.ifi_iqdrops,
        u64::from(m.ifmd_snd_drops),
    ])
}

/// Interface `index`'s `IFMIB_IFDATA` record, or `None` if the read fails or
/// the index now names another interface.
fn ifmib(index: u32, name: &CStr) -> Option<libc::ifmibdata> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_LINK,
        libc::NETLINK_GENERIC,
        libc::IFMIB_IFDATA,
        index as libc::c_int,
        libc::IFDATA_GENERAL,
    ];
    // SAFETY: `ifmibdata` is plain integers and bytes; all-zero is valid.
    let mut data: libc::ifmibdata = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ifmibdata>();

    // SAFETY: the kernel writes at most `len` bytes into `data`.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            (&mut data as *mut libc::ifmibdata).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::ifmibdata>() {
        return None;
    }

    // SAFETY: the kernel NUL-terminates `ifmd_name` within its IFNAMSIZ bytes.
    let read_name = unsafe { CStr::from_ptr(data.ifmd_name.as_ptr()) };
    (read_name == name).then_some(data)
}

/// The hardware-backed interfaces `network_host_*` sums and the interfaces
/// under `IOResources` (see the module doc), or `None` if the I/O registry
/// lookup, `if_nameindex` or opening a socket fails.
fn classify_interfaces() -> Option<Classes> {
    let key = CfString::new("BSD Name")?;
    let mut classes = Classes::default();
    for s in Service::matching("IONetworkInterface")? {
        let Some(name) = s.string(&key) else {
            continue;
        };
        if has_ancestor(&s, c"IOResources") {
            classes.software.insert(name);
        } else {
            classes.host.insert(name);
        }
    }

    let socket = ioctl_socket()?;
    for (_, name) in interface_names()? {
        let functional_type = ifreq_u32(&socket, &name, SIOCGIFFUNCTIONALTYPE);
        let delegate = ifreq_u32(&socket, &name, SIOCGIFDELEGATE);
        if let (Some(functional_type), Some(delegate)) = (functional_type, delegate) {
            if is_radio(functional_type, delegate) {
                classes.host.insert(name.to_string_lossy().into_owned());
            }
        }
    }
    Some(classes)
}

/// `IFRTYPE_FUNCTIONAL_WIFI_INFRA`, `_WIFI_AWDL` and `_CELLULAR` (`net/if.h`).
const RADIO_FUNCTIONAL_TYPES: [u32; 3] = [3, 4, 5];

/// True for an interface whose functional type is wifi or cellular and that
/// has no delegate interface. An interface with a delegate (a `utun` or
/// `ipsec` tunnel, or a redirect interface `rdN`) reports its delegate's
/// functional type and sends through the delegate, whose counters already
/// include its traffic. Both are read when the interface is first listed
/// and, for one in neither of the `Classes` sets then, again on the first
/// read at least `SETTLE` later and on each read after that until a
/// classification succeeds. An interface counted as host stays host, so a cellular
/// redirect interface listed before its delegate is set is counted as host
/// from then on.
fn is_radio(functional_type: u32, delegate: u32) -> bool {
    RADIO_FUNCTIONAL_TYPES.contains(&functional_type) && delegate == 0
}

/// `SIOCGIFFUNCTIONALTYPE` (`sys/sockio.h`): `_IOWR('i', 173, struct ifreq)`.
/// Writes the interface's `IFRTYPE_FUNCTIONAL_*` type into the request.
const SIOCGIFFUNCTIONALTYPE: libc::c_ulong = iowr_ifreq(173);
/// `SIOCGIFDELEGATE` (xnu `sys/sockio_private.h`, not in the SDK):
/// `_IOWR('i', 157, struct ifreq)`. Writes the index of the interface's
/// delegate, or 0 if it has none, into the request. The tests cannot check
/// 157: on `lo0` many other `'i'` numbers, among them 156, 160 and 162, also
/// succeed and read 0, and making an interface with a delegate needs root.
const SIOCGIFDELEGATE: libc::c_ulong = iowr_ifreq(157);

/// `_IOWR('i', n, struct ifreq)` (`sys/ioccom.h`).
const fn iowr_ifreq(n: libc::c_ulong) -> libc::c_ulong {
    const IOC_INOUT: libc::c_ulong = 0xc000_0000;
    let len = std::mem::size_of::<IfreqU32>() as libc::c_ulong;
    IOC_INOUT | ((len & 0x1fff) << 16) | ((b'i' as libc::c_ulong) << 8) | n
}

/// `struct ifreq` (`net/if.h`) as the two ioctls above use it: the name, then
/// a `u32` at the start of the 16-byte union (`ifru_functional_type`,
/// `ifru_delegated`).
#[repr(C)]
struct IfreqU32 {
    name: [libc::c_char; libc::IFNAMSIZ],
    value: u32,
    _rest: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<IfreqU32>() == 32);

/// A socket for interface ioctls, or `None` if one cannot be opened.
fn ioctl_socket() -> Option<OwnedFd> {
    // SAFETY: no preconditions; returns a descriptor or -1.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    // SAFETY: a non-negative `fd` is a descriptor this process now owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The `u32` that `request`, one of the ioctls above, sets for interface
/// `name`, or `None` if the ioctl fails.
fn ifreq_u32(socket: &OwnedFd, name: &CStr, request: libc::c_ulong) -> Option<u32> {
    let mut req = IfreqU32 {
        name: [0; libc::IFNAMSIZ],
        value: 0,
        _rest: [0; 12],
    };
    let bytes = name.to_bytes();
    if bytes.len() >= req.name.len() {
        return None;
    }
    for (dst, src) in req.name.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    // SAFETY: `req` has the size and layout of a `struct ifreq` with a
    // NUL-terminated name, which is what these ioctls read and write.
    let rc = unsafe { libc::ioctl(socket.as_raw_fd(), request, &mut req) };
    (rc == 0).then_some(req.value)
}

/// True if an ancestor of `service` in the service plane is of class
/// `class`.
fn has_ancestor(service: &Service, class: &CStr) -> bool {
    let mut current = service.parent();
    while let Some(entry) = current {
        if entry.conforms_to(class) {
            return true;
        }
        current = entry.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static CLOCK: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    /// `State::update` at `SETTLE` after this test's previous read, so an
    /// interface in neither set is settled by its next read.
    fn read(state: &mut State, readings: Vec<Reading>, classify: impl FnOnce() -> Option<Classes>) {
        let at = CLOCK.with(|c| {
            let at = c.get().map_or_else(Instant::now, |t| t + SETTLE);
            c.set(Some(at));
            at
        });
        state.update(readings, at, classify);
    }

    /// A reading with these traffic counters and no errors or drops.
    fn reading(index: u32, name: &str, traffic: Sums) -> Reading {
        with_faults(index, name, traffic, [0; 4])
    }

    fn with_faults(index: u32, name: &str, traffic: Sums, faults: Sums) -> Reading {
        let mut counters = [0; 8];
        counters[..4].copy_from_slice(&traffic);
        counters[4..].copy_from_slice(&faults);
        Reading {
            index,
            name: name.to_string(),
            counters: Some(counters),
        }
    }

    fn host(names: &[&str]) -> impl FnOnce() -> Option<Classes> {
        classes(names, &[])
    }

    fn classes(host: &[&str], software: &[&str]) -> impl FnOnce() -> Option<Classes> {
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        let classes = Classes {
            host: set(host),
            software: set(software),
        };
        move || Some(classes)
    }

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn never() -> Option<Classes> {
        panic!("classified before SETTLE")
    }

    #[test]
    fn an_interface_registered_late_under_io_resources_adds_no_errors_or_drops() {
        let t0 = Instant::now();
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], t0, classes(&[], &[]));
        // vmenet4 is listed before the I/O registry lists it.
        let vmenet = |drops| with_faults(30, "vmenet4", [0; 4], [0, 0, drops, 0]);
        let t1 = ms(t0, 10);
        state.update(
            vec![reading(1, "lo0", [0; 4]), vmenet(0xAAAA_AAAA)],
            t1,
            classes(&[], &[]),
        );
        state.update(
            vec![reading(1, "lo0", [0; 4]), vmenet(2 * 0xAAAA_AAAA)],
            ms(t0, 20),
            never,
        );
        // At SETTLE the classification fails, so it stays unsettled.
        state.update(
            vec![reading(1, "lo0", [0; 4]), vmenet(3 * 0xAAAA_AAAA)],
            t1 + SETTLE,
            || None,
        );
        assert!(matches!(state.seen[&30].faults, Faults::Unsettled { .. }));
        state.update(
            vec![reading(1, "lo0", [0; 4]), vmenet(4 * 0xAAAA_AAAA)],
            t1 + SETTLE + Duration::from_millis(10),
            classes(&[], &["vmenet4"]),
        );
        assert_eq!(state.seen[&30].faults, Faults::Excluded);
        assert_eq!(state.faults, [0; 4]);
    }

    #[test]
    fn an_interface_in_neither_set_adds_what_it_held_after_settle() {
        let t0 = Instant::now();
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], t0, classes(&[], &[]));
        let utun = |errors| with_faults(50, "utun9", [0; 4], [errors, 0, 0, 0]);
        let t1 = ms(t0, 10);
        state.update(
            vec![reading(1, "lo0", [0; 4]), utun(1)],
            t1,
            classes(&[], &[]),
        );
        state.update(
            vec![reading(1, "lo0", [0; 4]), utun(3)],
            ms(t0, 1000),
            never,
        );
        assert_eq!(state.faults, [0; 4]);
        state.update(
            vec![reading(1, "lo0", [0; 4]), utun(4)],
            t1 + SETTLE,
            classes(&[], &[]),
        );
        // Its whole count, since it was first listed after the first read.
        assert_eq!(state.faults, [4, 0, 0, 0]);
        // Settled: no classification on a later read.
        state.update(
            vec![reading(1, "lo0", [0; 4]), utun(6)],
            t1 + 3 * SETTLE,
            never,
        );
        assert_eq!(state.faults, [6, 0, 0, 0]);
    }

    #[test]
    fn an_unsettled_interface_keeps_what_it_held_through_an_unreadable_read() {
        let t0 = Instant::now();
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], t0, classes(&[], &[]));
        let t1 = ms(t0, 10);
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                with_faults(50, "utun9", [0; 4], [1, 0, 0, 0]),
            ],
            t1,
            classes(&[], &[]),
        );
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                Reading {
                    index: 50,
                    name: "utun9".to_string(),
                    counters: None,
                },
            ],
            ms(t0, 20),
            never,
        );
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                with_faults(50, "utun9", [0; 4], [5, 0, 0, 0]),
            ],
            t1 + SETTLE,
            classes(&[], &[]),
        );
        assert_eq!(state.faults, [5, 0, 0, 0]);
    }

    #[test]
    fn an_interface_registered_late_as_hardware_is_host_from_then_on() {
        let t0 = Instant::now();
        let mut state = State::default();
        let en = |bytes, errors| with_faults(40, "en12", [bytes, 0, 0, 0], [errors, 0, 0, 0]);
        state.update(vec![en(100, 1)], t0, classes(&[], &[]));
        state.update(vec![en(300, 2)], ms(t0, 10), never);
        state.update(vec![en(600, 4)], t0 + SETTLE, host(&["en12"]));
        assert_eq!(state.total, [500, 0, 0, 0]);
        assert_eq!(state.host, [300, 0, 0, 0]);
        assert_eq!(state.faults, [3, 0, 0, 0]);
    }

    #[test]
    fn errors_and_drops_leave_out_interfaces_under_io_resources() {
        let mut state = State::default();
        let classify = || classes(&["en0"], &["vmenet0", "vmenet1"]);
        read(
            &mut state,
            vec![
                with_faults(1, "lo0", [0; 4], [0, 0, 100, 0]),
                with_faults(14, "en0", [0; 4], [1, 2, 0, 3]),
                with_faults(27, "vmenet0", [0; 4], [0, 0, 0xAAAA_AAAA, 0]),
            ],
            classify(),
        );
        read(
            &mut state,
            vec![
                with_faults(1, "lo0", [0; 4], [0, 0, 8300, 0]),
                with_faults(14, "en0", [0; 4], [2, 4, 0, 6]),
                with_faults(27, "vmenet0", [10, 1, 0, 0], [0, 0, 2 * 0xAAAA_AAAA, 0]),
                with_faults(28, "vmenet1", [0; 4], [0, 0, 5 * 0xAAAA_AAAA, 7]),
            ],
            classify(),
        );
        assert_eq!(state.faults, [1, 2, 8200, 3]);
        assert_eq!(state.total, [10, 1, 0, 0]);
    }

    #[test]
    fn the_first_read_is_the_baseline() {
        let mut state = State::default();
        read(
            &mut state,
            vec![reading(1, "lo0", [100, 10, 100, 10])],
            host(&[]),
        );
        assert_eq!(state.total, [0; 4]);

        read(
            &mut state,
            vec![reading(1, "lo0", [150, 12, 170, 13])],
            host(&[]),
        );
        assert_eq!(state.total, [50, 2, 70, 3]);
        assert_eq!(state.host, [0; 4]);
    }

    #[test]
    fn host_sums_only_hardware_backed_interfaces() {
        let mut state = State::default();
        let first = || {
            vec![
                reading(1, "lo0", [0; 4]),
                reading(14, "en0", [0; 4]),
                reading(28, "bridge100", [0; 4]),
            ]
        };
        read(&mut state, first(), host(&["en0"]));
        read(
            &mut state,
            vec![
                reading(1, "lo0", [1000, 10, 1000, 10]),
                reading(14, "en0", [200, 2, 300, 3]),
                reading(28, "bridge100", [40, 1, 50, 1]),
            ],
            host(&["en0"]),
        );
        assert_eq!(state.total, [1240, 13, 1350, 14]);
        assert_eq!(state.host, [200, 2, 300, 3]);
    }

    #[test]
    fn an_interface_that_goes_away_keeps_what_it_added() {
        let mut state = State::default();
        read(
            &mut state,
            vec![reading(1, "lo0", [0; 4]), reading(27, "vmenet0", [0; 4])],
            host(&[]),
        );
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                reading(27, "vmenet0", [500, 5, 0, 0]),
            ],
            host(&[]),
        );
        read(&mut state, vec![reading(1, "lo0", [0; 4])], host(&[]));
        assert_eq!(state.total, [500, 5, 0, 0]);
        assert!(!state.seen.contains_key(&27));
    }

    #[test]
    fn an_interface_that_appears_later_adds_its_whole_count() {
        let mut state = State::default();
        read(&mut state, vec![reading(1, "lo0", [0; 4])], host(&[]));
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                with_faults(40, "en12", [700, 7, 0, 0], [0, 0, 0, 5]),
            ],
            host(&["en12"]),
        );
        assert_eq!(state.total, [700, 7, 0, 0]);
        assert_eq!(state.host, [700, 7, 0, 0]);
        assert_eq!(state.faults, [0, 0, 0, 5]);
    }

    #[test]
    fn a_reused_index_is_a_new_interface() {
        let mut state = State::default();
        read(
            &mut state,
            vec![reading(27, "vmenet0", [900, 9, 0, 0])],
            host(&[]),
        );
        // Index 27 now names a hardware interface whose counters are lower.
        read(
            &mut state,
            vec![reading(27, "en12", [100, 1, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [100, 1, 0, 0]);
        assert_eq!(state.host, [100, 1, 0, 0]);
    }

    #[test]
    fn a_counter_that_went_down_counts_from_zero() {
        let mut state = State::default();
        read(
            &mut state,
            vec![reading(14, "en0", [900, 9, 0, 0])],
            host(&[]),
        );
        read(
            &mut state,
            vec![reading(14, "en0", [100, 1, 0, 0])],
            host(&[]),
        );
        assert_eq!(state.total, [100, 1, 0, 0]);
    }

    #[test]
    fn an_unreadable_interface_keeps_its_last_counters() {
        let mut state = State::default();
        read(
            &mut state,
            vec![reading(14, "en0", [100, 1, 0, 0])],
            host(&["en0"]),
        );
        read(
            &mut state,
            vec![Reading {
                index: 14,
                name: "en0".to_string(),
                counters: None,
            }],
            || panic!("no new interface, so no registry lookup"),
        );
        read(
            &mut state,
            vec![reading(14, "en0", [300, 3, 0, 0])],
            host(&["en0"]),
        );
        assert_eq!(state.total, [200, 2, 0, 0]);
        assert_eq!(state.host, [200, 2, 0, 0]);
    }

    #[test]
    fn an_interface_unreadable_at_the_first_read_takes_its_first_counters_as_its_baseline() {
        let mut state = State::default();
        let unreadable = || Reading {
            index: 40,
            name: "en12".to_string(),
            counters: None,
        };
        read(&mut state, vec![unreadable()], host(&["en12"]));
        read(&mut state, vec![unreadable()], host(&["en12"]));
        read(
            &mut state,
            vec![reading(40, "en12", [5_000_000_000, 5, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [0; 4]);
        read(
            &mut state,
            vec![reading(40, "en12", [5_000_000_100, 6, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [100, 1, 0, 0]);
        assert_eq!(state.host, [100, 1, 0, 0]);
    }

    #[test]
    fn an_interface_unreadable_when_first_listed_later_adds_its_whole_count() {
        let mut state = State::default();
        read(&mut state, vec![reading(1, "lo0", [0; 4])], host(&[]));
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                Reading {
                    index: 40,
                    name: "en12".to_string(),
                    counters: None,
                },
            ],
            host(&[]),
        );
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                reading(40, "en12", [700, 7, 0, 0]),
            ],
            host(&[]),
        );
        assert_eq!(state.total, [700, 7, 0, 0]);
    }

    #[test]
    fn a_failed_registry_lookup_keeps_a_baseline_interface_pending() {
        let mut state = State::default();
        read(
            &mut state,
            vec![Reading {
                index: 40,
                name: "en12".to_string(),
                counters: None,
            }],
            host(&["en12"]),
        );
        read(
            &mut state,
            vec![reading(40, "en12", [5_000_000_000, 5, 0, 0])],
            || None,
        );
        read(
            &mut state,
            vec![reading(40, "en12", [5_000_000_100, 6, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [0; 4]);
    }

    #[test]
    fn a_failed_registry_lookup_leaves_new_interfaces_for_the_next_read() {
        let mut state = State::default();
        read(&mut state, vec![reading(1, "lo0", [0; 4])], host(&[]));
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                reading(40, "en12", [700, 7, 0, 0]),
            ],
            || None,
        );
        assert_eq!(state.total, [0; 4]);
        read(
            &mut state,
            vec![
                reading(1, "lo0", [0; 4]),
                reading(40, "en12", [800, 8, 0, 0]),
            ],
            host(&["en12"]),
        );
        assert_eq!(state.total, [800, 8, 0, 0]);
        assert_eq!(state.host, [800, 8, 0, 0]);
    }

    // The snapshot routes these series to the group registered under their
    // module's sampler, so both must be `network_traffic`.
    #[test]
    fn stats_attribute_to_this_sampler() {
        let module = module_path!().trim_end_matches("::tests");
        let modules = crate::agent::samplers::sampler_modules();
        assert_eq!(
            crate::agent::samplers::attribute_sampler(&format!("{module}::stats"), &modules),
            NAME
        );
        assert_eq!(COUNTERS_ACQ.sampler, NAME);
    }

    // `IFRTYPE_FUNCTIONAL_LOOPBACK` and no delegate: checks `iowr_ifreq`,
    // the functional-type command and the `ifreq` layout, which a wrong value
    // of any would fail silently. See `SIOCGIFDELEGATE` for what it cannot
    // check.
    #[test]
    fn loopback_reads_as_functional_type_loopback_with_no_delegate() {
        let socket = ioctl_socket().unwrap();
        assert_eq!(ifreq_u32(&socket, c"lo0", SIOCGIFFUNCTIONALTYPE), Some(1));
        assert_eq!(ifreq_u32(&socket, c"lo0", SIOCGIFDELEGATE), Some(0));
        assert_eq!(SIOCGIFFUNCTIONALTYPE, 0xc020_69ad);
    }

    // Functional types and delegates measured on a Mac: en0 and ap1 (3, 0),
    // awdl0 (4, 0), a bridge (2, 0); a tunnel delegated to en0 (index 14)
    // reports en0's type.
    #[test]
    fn radio_is_a_wifi_or_cellular_type_with_no_delegate() {
        assert!(is_radio(3, 0));
        assert!(is_radio(4, 0));
        assert!(is_radio(5, 0));
        assert!(!is_radio(3, 14));
        assert!(!is_radio(2, 0));
        assert!(!is_radio(1, 0));
    }

    // Checks the interfaces this host has: `awdl0` is not an
    // `IONetworkInterface`, so only the functional-type rule can add it.
    #[test]
    fn awdl_is_host_when_listed() {
        let listed = interface_names()
            .unwrap()
            .iter()
            .any(|(_, name)| name.as_c_str() == c"awdl0");
        if listed {
            assert!(classify_interfaces().unwrap().host.contains("awdl0"));
        }
    }

    #[test]
    fn a_read_whose_name_does_not_match_is_refused() {
        // SAFETY: the name is NUL-terminated.
        let index = unsafe { libc::if_nametoindex(c"lo0".as_ptr()) };
        assert!(index > 0);
        assert!(counters(index, c"lo0").is_some());
        assert!(counters(index, c"en999").is_none());
    }

    #[test]
    fn loopback_is_counted_and_is_not_host() {
        let classes = classify_interfaces().expect("registry lookup");
        assert!(!classes.host.contains("lo0"));
        assert!(!classes.software.contains("lo0"));

        let interfaces = read_interfaces().expect("if_nameindex");
        assert!(interfaces
            .iter()
            .any(|r| r.name == "lo0" && r.counters.is_some()));
    }

    // Checks the interfaces this host has: on one with no software
    // interface (a `vmenet` of a running VM, for example) the software
    // assertions have nothing to check.
    #[test]
    fn interfaces_under_io_resources_are_software_and_not_host() {
        let key = CfString::new("BSD Name").unwrap();
        let classes = classify_interfaces().unwrap();
        for s in Service::matching("IONetworkInterface").unwrap() {
            assert!(has_ancestor(&s, c"IOPlatformExpertDevice"));
            let name = s.string(&key).unwrap();
            if has_ancestor(&s, c"IOResources") {
                assert!(!classes.host.contains(&name));
                assert!(classes.software.contains(&name));
            } else {
                assert!(!classes.software.contains(&name));
            }
        }
    }

    // The one test in this module that refreshes the sampler: two would race
    // on the process-global counters it sets.
    #[tokio::test]
    async fn refresh_counts_loopback_traffic_and_publishes_errors_and_drops() {
        let sampler = init(Arc::new(toml::from_str("").unwrap()))
            .unwrap()
            .unwrap();
        let faults = [
            &NETWORK_RX_ERRORS,
            &NETWORK_TX_ERRORS,
            &NETWORK_RX_DROPS,
            &NETWORK_TX_DROPS,
        ];
        for counter in faults {
            counter.set(u64::MAX);
        }
        sampler.refresh().await;
        for counter in faults {
            assert!(counter.value() < u64::MAX);
        }
        let before = NETWORK_TX_BYTES.value();

        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = socket.local_addr().unwrap();
        let payload = [0u8; 1000];
        for _ in 0..100 {
            socket.send_to(&payload, target).unwrap();
        }

        sampler.refresh().await;
        assert!(COUNTERS_ACQ.window().is_some());
        let sent = NETWORK_TX_BYTES.value() - before;
        assert!(sent >= 100_000, "transmitted {sent} bytes");
    }
}
