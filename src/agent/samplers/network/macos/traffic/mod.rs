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
//! The counters start at zero when the sampler starts and add each
//! interface's increase since the previous read. An interface first listed
//! after the sampler started adds its whole count, since it was created
//! after the previous read. What an interface added stays in the sums after
//! it goes away; its traffic after the last read before it went away is not
//! counted.
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

mod stats {
    include!("../../linux/traffic/stats.rs");
}

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
    let Some(host) = host_interfaces() else {
        return Err(crate::agent::sampler_status::Unsupported(
            "listing the hardware-backed interfaces failed (I/O registry, if_nameindex or \
             socket)"
                .to_string(),
        )
        .into());
    };

    let mut state = State::default();
    state.update(interfaces, || Some(host));

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
        // The window is the counter read; a registry walk for a new
        // interface in `update` is not part of it.
        guard.mark_end();
        let mut state = self.state.lock().unwrap();
        state.update(interfaces, host_interfaces);

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

        guard.finish();
    }
}

/// Cumulative counters: received bytes, received packets, transmitted bytes,
/// transmitted packets.
type Counters = [u64; 4];

/// One interface listed by `if_nameindex`, with its counters, or `None` if
/// they could not be read.
struct Reading {
    index: u32,
    name: String,
    counters: Option<Counters>,
}

/// An interface as of the last read.
struct Seen {
    name: String,
    host: bool,
    last: Counters,
}

#[derive(Default)]
struct State {
    /// The interfaces of the last read, by index.
    seen: HashMap<u32, Seen>,
    /// The sums over every interface and over hardware-backed interfaces.
    total: Counters,
    host: Counters,
    /// False until the first read, whose counters are the baseline.
    started: bool,
    /// Interfaces listed but unreadable in every read since the first: their
    /// first readable counters are their baseline.
    baseline_pending: HashSet<(u32, String)>,
}

impl State {
    /// Add each interface's increase since the last read. `host` returns the
    /// names of the hardware-backed interfaces; it is called at most once,
    /// and only if an interface not seen before is listed. If it returns
    /// `None`, such interfaces are left out of this read and are new again in
    /// the next.
    fn update(&mut self, readings: Vec<Reading>, host: impl FnOnce() -> Option<HashSet<String>>) {
        let mut host = Some(host);
        let mut host_names: Option<Option<HashSet<String>>> = None;
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

            let (is_host, increase) = match previous {
                Some(s) => (s.host, increase(&now, &s.last)),
                None => {
                    let names = host_names
                        .get_or_insert_with(|| host.take().and_then(|f| f()))
                        .as_ref();
                    let Some(names) = names else {
                        if in_baseline {
                            pending.insert((index, name));
                        }
                        continue;
                    };
                    let increase = if in_baseline { [0; 4] } else { now };
                    (names.contains(&name), increase)
                }
            };

            add(&mut self.total, &increase);
            if is_host {
                add(&mut self.host, &increase);
            }
            next.insert(
                index,
                Seen {
                    name,
                    host: is_host,
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

fn add(sum: &mut Counters, increase: &Counters) {
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
/// index now names another interface.
fn counters(index: u32, name: &CStr) -> Option<Counters> {
    let d = ifmib(index, name)?.ifmd_data;
    Some([d.ifi_ibytes, d.ifi_ipackets, d.ifi_obytes, d.ifi_opackets])
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

/// The names of the interfaces `network_host_*` sums (see the module doc),
/// or `None` if the I/O registry lookup, `if_nameindex` or opening a socket
/// fails.
fn host_interfaces() -> Option<HashSet<String>> {
    let key = CfString::new("BSD Name")?;
    let interfaces = Service::matching("IONetworkInterface")?;
    let mut out: HashSet<String> = interfaces
        .into_iter()
        .filter(|s| !has_ancestor(s, c"IOResources"))
        .filter_map(|s| s.string(&key))
        .collect();

    let socket = ioctl_socket()?;
    for (_, name) in interface_names()? {
        let functional_type = ifreq_u32(&socket, &name, SIOCGIFFUNCTIONALTYPE);
        let delegate = ifreq_u32(&socket, &name, SIOCGIFDELEGATE);
        if let (Some(functional_type), Some(delegate)) = (functional_type, delegate) {
            if is_radio(functional_type, delegate) {
                out.insert(name.to_string_lossy().into_owned());
            }
        }
    }
    Some(out)
}

/// `IFRTYPE_FUNCTIONAL_WIFI_INFRA`, `_WIFI_AWDL` and `_CELLULAR` (`net/if.h`).
const RADIO_FUNCTIONAL_TYPES: [u32; 3] = [3, 4, 5];

/// True for an interface whose functional type is wifi or cellular and that
/// has no delegate interface. An interface with a delegate (a `utun` or
/// `ipsec` tunnel, or a redirect interface `rdN`) reports its delegate's
/// functional type and sends through the delegate, whose counters already
/// include its traffic. Both are read once, when the interface is first
/// listed: a cellular redirect interface listed before its delegate is set
/// is counted as host from then on.
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

    fn reading(index: u32, name: &str, counters: Counters) -> Reading {
        Reading {
            index,
            name: name.to_string(),
            counters: Some(counters),
        }
    }

    fn host(names: &[&str]) -> impl FnOnce() -> Option<HashSet<String>> {
        let names: HashSet<String> = names.iter().map(|n| n.to_string()).collect();
        move || Some(names)
    }

    #[test]
    fn the_first_read_is_the_baseline() {
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [100, 10, 100, 10])], host(&[]));
        assert_eq!(state.total, [0; 4]);

        state.update(vec![reading(1, "lo0", [150, 12, 170, 13])], host(&[]));
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
        state.update(first(), host(&["en0"]));
        state.update(
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
        state.update(
            vec![reading(1, "lo0", [0; 4]), reading(27, "vmenet0", [0; 4])],
            host(&[]),
        );
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                reading(27, "vmenet0", [500, 5, 0, 0]),
            ],
            host(&[]),
        );
        state.update(vec![reading(1, "lo0", [0; 4])], host(&[]));
        assert_eq!(state.total, [500, 5, 0, 0]);
        assert!(!state.seen.contains_key(&27));
    }

    #[test]
    fn an_interface_that_appears_later_adds_its_whole_count() {
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], host(&[]));
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                reading(40, "en12", [700, 7, 0, 0]),
            ],
            host(&["en12"]),
        );
        assert_eq!(state.total, [700, 7, 0, 0]);
        assert_eq!(state.host, [700, 7, 0, 0]);
    }

    #[test]
    fn a_reused_index_is_a_new_interface() {
        let mut state = State::default();
        state.update(vec![reading(27, "vmenet0", [900, 9, 0, 0])], host(&[]));
        // Index 27 now names a hardware interface whose counters are lower.
        state.update(vec![reading(27, "en12", [100, 1, 0, 0])], host(&["en12"]));
        assert_eq!(state.total, [100, 1, 0, 0]);
        assert_eq!(state.host, [100, 1, 0, 0]);
    }

    #[test]
    fn a_counter_that_went_down_counts_from_zero() {
        let mut state = State::default();
        state.update(vec![reading(14, "en0", [900, 9, 0, 0])], host(&[]));
        state.update(vec![reading(14, "en0", [100, 1, 0, 0])], host(&[]));
        assert_eq!(state.total, [100, 1, 0, 0]);
    }

    #[test]
    fn an_unreadable_interface_keeps_its_last_counters() {
        let mut state = State::default();
        state.update(vec![reading(14, "en0", [100, 1, 0, 0])], host(&["en0"]));
        state.update(
            vec![Reading {
                index: 14,
                name: "en0".to_string(),
                counters: None,
            }],
            || panic!("no new interface, so no registry lookup"),
        );
        state.update(vec![reading(14, "en0", [300, 3, 0, 0])], host(&["en0"]));
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
        state.update(vec![unreadable()], host(&["en12"]));
        state.update(vec![unreadable()], host(&["en12"]));
        state.update(
            vec![reading(40, "en12", [5_000_000_000, 5, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [0; 4]);
        state.update(
            vec![reading(40, "en12", [5_000_000_100, 6, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [100, 1, 0, 0]);
        assert_eq!(state.host, [100, 1, 0, 0]);
    }

    #[test]
    fn an_interface_unreadable_when_first_listed_later_adds_its_whole_count() {
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], host(&[]));
        state.update(
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
        state.update(
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
        state.update(
            vec![Reading {
                index: 40,
                name: "en12".to_string(),
                counters: None,
            }],
            host(&["en12"]),
        );
        state.update(vec![reading(40, "en12", [5_000_000_000, 5, 0, 0])], || None);
        state.update(
            vec![reading(40, "en12", [5_000_000_100, 6, 0, 0])],
            host(&["en12"]),
        );
        assert_eq!(state.total, [0; 4]);
    }

    #[test]
    fn a_failed_registry_lookup_leaves_new_interfaces_for_the_next_read() {
        let mut state = State::default();
        state.update(vec![reading(1, "lo0", [0; 4])], host(&[]));
        state.update(
            vec![
                reading(1, "lo0", [0; 4]),
                reading(40, "en12", [700, 7, 0, 0]),
            ],
            || None,
        );
        assert_eq!(state.total, [0; 4]);
        state.update(
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
            assert!(host_interfaces().unwrap().contains("awdl0"));
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
        let host = host_interfaces().expect("registry lookup");
        assert!(!host.contains("lo0"));

        let interfaces = read_interfaces().expect("if_nameindex");
        assert!(interfaces
            .iter()
            .any(|r| r.name == "lo0" && r.counters.is_some()));
    }

    // Checks the interfaces this host has: on one with no software
    // interface (a `vmenet` of a running VM, for example) the second
    // assertion has nothing to check.
    #[test]
    fn interfaces_under_io_resources_are_not_host() {
        let key = CfString::new("BSD Name").unwrap();
        let host = host_interfaces().unwrap();
        for s in Service::matching("IONetworkInterface").unwrap() {
            assert!(has_ancestor(&s, c"IOPlatformExpertDevice"));
            if has_ancestor(&s, c"IOResources") {
                assert!(!host.contains(&s.string(&key).unwrap()));
            }
        }
    }

    #[tokio::test]
    async fn refresh_counts_loopback_traffic() {
        let sampler = init(Arc::new(toml::from_str("").unwrap()))
            .unwrap()
            .unwrap();
        sampler.refresh().await;
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
