//! Bindings for IOReport, the macOS framework `powermetrics` reads its
//! energy and performance-state counters from, for the `cpu_power`,
//! `cpu_frequency` and `gpu_apple` samplers.
//!
//! IOReport is private: it ships in the shared cache as `libIOReport.dylib`
//! with no header, and Apple documents none of it. The signatures here are
//! the ones open-source tools that read it use, and only what the three
//! samplers call is bound. It needs no privileges. A subscription's samples
//! are cumulative since boot: a simple channel's value is a running total,
//! and a state channel's residencies are running totals of 24 MHz system
//! counter ticks.

use std::ffi::{c_char, c_void, CString};
use std::sync::OnceLock;

use libloading::Library;

type CfTypeRef = *const c_void;

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// `IOReportChannelGetFormat`'s value for a channel with one integer.
const FORMAT_SIMPLE: i32 = 1;
/// `IOReportChannelGetFormat`'s value for a channel with a residency per
/// state.
const FORMAT_STATE: i32 = 2;

/// Where IOReport lives. The file is in the dyld shared cache, not on
/// disk, and `dlopen` finds it there.
const LIBRARY: &str = "/usr/lib/libIOReport.dylib";

/// The IOReport functions the samplers call, resolved from the library
/// loaded at runtime. IOReport is loaded rather than linked so that a macOS
/// release without it, or without one of these functions, leaves the three
/// samplers unsupported instead of stopping the binary from starting.
pub(crate) struct Api {
    copy_channels_in_group: unsafe extern "C" fn(CfTypeRef, CfTypeRef, u64, u64, u64) -> CfTypeRef,
    merge_channels: unsafe extern "C" fn(CfTypeRef, CfTypeRef, CfTypeRef),
    create_subscription:
        unsafe extern "C" fn(*const c_void, CfTypeRef, *mut CfTypeRef, u64, CfTypeRef) -> CfTypeRef,
    create_samples: unsafe extern "C" fn(CfTypeRef, CfTypeRef, CfTypeRef) -> CfTypeRef,
    channel_get_group: unsafe extern "C" fn(CfTypeRef) -> CfTypeRef,
    channel_get_subgroup: unsafe extern "C" fn(CfTypeRef) -> CfTypeRef,
    channel_get_channel_name: unsafe extern "C" fn(CfTypeRef) -> CfTypeRef,
    channel_get_unit_label: unsafe extern "C" fn(CfTypeRef) -> CfTypeRef,
    channel_get_format: unsafe extern "C" fn(CfTypeRef) -> i32,
    simple_get_integer_value: unsafe extern "C" fn(CfTypeRef, *mut i32) -> i64,
    state_get_count: unsafe extern "C" fn(CfTypeRef) -> i32,
    state_get_name_for_index: unsafe extern "C" fn(CfTypeRef, i32) -> CfTypeRef,
    state_get_residency: unsafe extern "C" fn(CfTypeRef, i32) -> i64,
    // The function pointers above are valid while the library is loaded.
    // An `Api` is only kept in the `API` static, which is never dropped.
    _library: Library,
}

impl Api {
    fn load(path: &str) -> Result<Self, String> {
        // SAFETY: loading a system library runs its initializers, which is
        // what linking it did.
        let library =
            unsafe { Library::new(path) }.map_err(|e| format!("could not load {path}: {e}"))?;
        // SAFETY: each type is the signature the function is called with
        // (see the module docs on where the signatures come from). The
        // pointer is copied out of the `Symbol`, and the library it points
        // into is kept in the returned `Api`.
        unsafe fn get<T: Copy>(library: &Library, name: &str) -> Result<T, String> {
            library
                .get::<T>(name.as_bytes())
                .map(|f| *f)
                .map_err(|e| format!("{name}: {e}"))
        }
        // SAFETY: as for `get`.
        unsafe {
            Ok(Api {
                copy_channels_in_group: get(&library, "IOReportCopyChannelsInGroup")?,
                merge_channels: get(&library, "IOReportMergeChannels")?,
                create_subscription: get(&library, "IOReportCreateSubscription")?,
                create_samples: get(&library, "IOReportCreateSamples")?,
                channel_get_group: get(&library, "IOReportChannelGetGroup")?,
                channel_get_subgroup: get(&library, "IOReportChannelGetSubGroup")?,
                channel_get_channel_name: get(&library, "IOReportChannelGetChannelName")?,
                channel_get_unit_label: get(&library, "IOReportChannelGetUnitLabel")?,
                channel_get_format: get(&library, "IOReportChannelGetFormat")?,
                simple_get_integer_value: get(&library, "IOReportSimpleGetIntegerValue")?,
                state_get_count: get(&library, "IOReportStateGetCount")?,
                state_get_name_for_index: get(&library, "IOReportStateGetNameForIndex")?,
                state_get_residency: get(&library, "IOReportStateGetResidency")?,
                _library: library,
            })
        }
    }
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

/// IOReport, loaded on first use. The error says which file could not be
/// loaded or which function it lacks.
pub(crate) fn api() -> Result<&'static Api, &'static str> {
    API.get_or_init(|| Api::load(LIBRARY))
        .as_ref()
        .map_err(String::as_str)
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: CfTypeRef, s: *const c_char, encoding: u32) -> CfTypeRef;
    fn CFStringGetCString(s: CfTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFDictionaryGetValue(dict: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn CFDictionaryGetCount(dict: CfTypeRef) -> isize;
    fn CFDictionaryCreateMutableCopy(
        alloc: CfTypeRef,
        capacity: isize,
        dict: CfTypeRef,
    ) -> CfTypeRef;
    fn CFArrayGetCount(array: CfTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: isize) -> CfTypeRef;
    fn CFArrayCreateMutable(
        alloc: CfTypeRef,
        capacity: isize,
        callbacks: *const c_void,
    ) -> CfTypeRef;
    fn CFArrayAppendValue(array: CfTypeRef, value: CfTypeRef);
    fn CFDictionarySetValue(dict: CfTypeRef, key: CfTypeRef, value: CfTypeRef);
    fn CFRelease(cf: CfTypeRef);
    static kCFTypeArrayCallBacks: c_void;
}

/// The key of a channel dictionary's, or a sample's, array of channels.
const CHANNELS_KEY: &str = "IOReportChannels";

/// An owned CoreFoundation object, released on drop.
struct Owned(CfTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `Owned` is only built from a non-null +1 reference.
        unsafe { CFRelease(self.0) }
    }
}

fn cf_string(s: &str) -> Option<Owned> {
    let c = CString::new(s).ok()?;
    // SAFETY: `c` is a valid NUL-terminated UTF-8 string for the call.
    let r =
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_STRING_ENCODING_UTF8) };
    (!r.is_null()).then_some(Owned(r))
}

/// The text of a CFString, or an empty string.
///
/// # Safety
///
/// `s` must be null or a live CFString.
unsafe fn text(s: CfTypeRef) -> String {
    if s.is_null() {
        return String::new();
    }
    let mut buf = [0 as c_char; 256];
    if CFStringGetCString(
        s,
        buf.as_mut_ptr(),
        buf.len() as isize,
        CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return String::new();
    }
    std::ffi::CStr::from_ptr(buf.as_ptr())
        .to_string_lossy()
        .into_owned()
}

/// One channel of a sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Channel {
    pub group: String,
    pub subgroup: String,
    pub name: String,
    /// The unit of a simple channel's value, such as `mJ` or `nJ`.
    pub unit: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    /// A running total.
    Simple(i64),
    /// The running residency of each state, by state name, in 24 MHz ticks.
    State(Vec<(String, i64)>),
}

/// A subscription to some IOReport channel groups.
pub(crate) struct Subscription {
    api: &'static Api,
    subscription: Owned,
    subscribed: Owned,
}

// SAFETY: the subscription and its channel dictionary are CoreFoundation
// objects, which may be used from any thread one at a time; the samplers hold
// a `Subscription` behind a mutex.
unsafe impl Send for Subscription {}

impl Subscription {
    /// Subscribe to every channel of each `(group, subgroup)`; a `None`
    /// subgroup takes the whole group. `None` when a group has no channels
    /// on this machine or the subscription fails.
    pub(crate) fn new(api: &'static Api, groups: &[(&str, Option<&str>)]) -> Option<Self> {
        Self::filtered(api, groups, |_| true)
    }

    /// Subscribe to the channels of each `(group, subgroup)` whose name
    /// `keep` accepts. A sample's cost grows with the channels subscribed:
    /// the 328 channels of `Energy Model` take about 3.7 ms on an M4 Max.
    /// `None` when no channel is kept or the subscription fails.
    pub(crate) fn filtered(
        api: &'static Api,
        groups: &[(&str, Option<&str>)],
        keep: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        let mut merged: Option<Owned> = None;
        for (group, subgroup) in groups {
            let group = cf_string(group)?;
            let subgroup = match subgroup {
                Some(s) => Some(cf_string(s)?),
                None => None,
            };
            // SAFETY: the strings are live; a null subgroup selects every
            // subgroup. The result is a +1 dictionary or null.
            let channels = unsafe {
                (api.copy_channels_in_group)(
                    group.0,
                    subgroup.as_ref().map_or(std::ptr::null(), |s| s.0),
                    0,
                    0,
                    0,
                )
            };
            if channels.is_null() {
                return None;
            }
            let channels = filter(api, &Owned(channels), &keep)?;
            match &merged {
                // SAFETY: both are live channel dictionaries; the first is
                // a mutable copy, which the merge adds to.
                Some(m) => unsafe { (api.merge_channels)(m.0, channels.0, std::ptr::null()) },
                None => merged = Some(channels),
            }
        }
        let desired = merged?;
        // SAFETY: `desired` is a live dictionary.
        let empty = unsafe {
            let key = cf_string(CHANNELS_KEY)?;
            let array = CFDictionaryGetValue(desired.0, key.0);
            array.is_null() || CFArrayGetCount(array) == 0
        };
        if empty {
            return None;
        }
        let mut subscribed: CfTypeRef = std::ptr::null();
        // SAFETY: `desired` is a live mutable channel dictionary and
        // `subscribed` a valid out-pointer; both results are +1 or null.
        let subscription = unsafe {
            (api.create_subscription)(
                std::ptr::null(),
                desired.0,
                &mut subscribed,
                0,
                std::ptr::null(),
            )
        };
        if subscription.is_null() || subscribed.is_null() {
            if !subscription.is_null() {
                drop(Owned(subscription));
            }
            if !subscribed.is_null() {
                drop(Owned(subscribed));
            }
            return None;
        }
        Some(Subscription {
            api,
            subscription: Owned(subscription),
            subscribed: Owned(subscribed),
        })
    }

    /// Every subscribed channel's running totals. Simple and state channels
    /// are returned; other formats are left out.
    pub(crate) fn sample(&self) -> Option<Vec<Channel>> {
        // SAFETY: both objects are live; the result is +1 or null.
        let api = self.api;
        let samples = unsafe {
            (api.create_samples)(self.subscription.0, self.subscribed.0, std::ptr::null())
        };
        if samples.is_null() {
            return None;
        }
        let samples = Owned(samples);
        let key = cf_string(CHANNELS_KEY)?;
        let mut out = Vec::new();
        // SAFETY: `samples` is a live dictionary; the array and its items are
        // borrowed from it (Get rule) and only read while it is alive.
        unsafe {
            let array = CFDictionaryGetValue(samples.0, key.0);
            if array.is_null() {
                return None;
            }
            for i in 0..CFArrayGetCount(array) {
                let item = CFArrayGetValueAtIndex(array, i);
                let value = match (api.channel_get_format)(item) {
                    FORMAT_SIMPLE => {
                        Value::Simple((api.simple_get_integer_value)(item, std::ptr::null_mut()))
                    }
                    FORMAT_STATE => Value::State(
                        (0..(api.state_get_count)(item))
                            .map(|s| {
                                (
                                    text((api.state_get_name_for_index)(item, s)),
                                    (api.state_get_residency)(item, s),
                                )
                            })
                            .collect(),
                    ),
                    _ => continue,
                };
                out.push(Channel {
                    group: text((api.channel_get_group)(item)),
                    subgroup: text((api.channel_get_subgroup)(item)),
                    name: text((api.channel_get_channel_name)(item)),
                    unit: text((api.channel_get_unit_label)(item)),
                    value,
                });
            }
        }
        Some(out)
    }
}

/// A mutable copy of the channel dictionary `channels` holding only the
/// channels whose name `keep` accepts.
fn filter(api: &Api, channels: &Owned, keep: &impl Fn(&str) -> bool) -> Option<Owned> {
    let key = cf_string(CHANNELS_KEY)?;
    // SAFETY: `channels` is a live dictionary; the copy and the new array
    // are +1, and the items appended are borrowed from the original array,
    // which the new array retains through `kCFTypeArrayCallBacks`.
    unsafe {
        let copy = CFDictionaryCreateMutableCopy(
            std::ptr::null(),
            CFDictionaryGetCount(channels.0),
            channels.0,
        );
        if copy.is_null() {
            return None;
        }
        let copy = Owned(copy);
        let array = CFDictionaryGetValue(channels.0, key.0);
        if array.is_null() {
            return None;
        }
        let kept = CFArrayCreateMutable(std::ptr::null(), 0, &kCFTypeArrayCallBacks);
        if kept.is_null() {
            return None;
        }
        let kept = Owned(kept);
        for i in 0..CFArrayGetCount(array) {
            let item = CFArrayGetValueAtIndex(array, i);
            if keep(&text((api.channel_get_channel_name)(item))) {
                CFArrayAppendValue(kept.0, item);
            }
        }
        CFDictionarySetValue(copy.0, key.0, kept.0);
        Some(copy)
    }
}

/// Whether this process runs in a virtual machine, whose guest may have no
/// IOReport energy or performance-state channels and no device-tree
/// frequency tables. The tests that read this machine skip there and fail
/// on bare metal.
#[cfg(test)]
pub(crate) fn in_vm() -> bool {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `v`/`len` are valid
    // out-pointers of the sysctl's size.
    let r = unsafe {
        libc::sysctlbyname(
            c"kern.hv_vmm_present".as_ptr(),
            (&mut v as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    r == 0 && v == 1
}

/// Skip a test that reads this machine's hardware when in a VM.
#[cfg(test)]
macro_rules! skip_in_vm {
    () => {
        if $crate::agent::samplers::ioreport::in_vm() {
            eprintln!("in a virtual machine; skipping");
            return;
        }
    };
}
#[cfg(test)]
pub(crate) use skip_in_vm;

#[cfg(test)]
mod tests {
    use super::*;

    /// IOReport loads from the shared cache, with every function the
    /// samplers call.
    #[test]
    fn the_library_loads() {
        api().unwrap();
    }

    /// A missing library is an error naming the file, not a failure to
    /// start.
    #[test]
    fn a_missing_library_is_an_error() {
        let Err(e) = Api::load("/usr/lib/libDoesNotExist.dylib") else {
            panic!("loaded a missing library");
        };
        assert!(e.contains("/usr/lib/libDoesNotExist.dylib"), "{e}");
    }

    /// A library without the functions is an error naming the first one
    /// missing.
    #[test]
    fn a_missing_function_is_an_error() {
        let Err(e) = Api::load("/usr/lib/libSystem.B.dylib") else {
            panic!("libSystem has IOReport's functions");
        };
        assert!(e.starts_with("IOReportCopyChannelsInGroup:"), "{e}");
    }

    /// The energy model is readable unprivileged and its counters only grow.
    #[test]
    fn energy_model_totals_are_running_totals() {
        skip_in_vm!();
        let sub = Subscription::new(api().unwrap(), &[("Energy Model", None)]).expect("subscribe");
        let total = |channels: &[Channel]| {
            channels
                .iter()
                .find(|c| c.name == "CPU Energy")
                .and_then(|c| match c.value {
                    Value::Simple(v) => Some(v),
                    _ => None,
                })
                .expect("CPU Energy")
        };
        let first = total(&sub.sample().expect("sample"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = total(&sub.sample().expect("sample"));
        assert!(first > 0, "a running total since boot");
        assert!(second >= first);
    }

    /// Each core's performance-state residencies sum to about the system
    /// counter's ticks since boot.
    #[test]
    fn core_residencies_are_running_ticks() {
        skip_in_vm!();
        let sub = Subscription::new(
            api().unwrap(),
            &[("CPU Stats", Some("CPU Core Performance States"))],
        )
        .expect("subscribe");
        let channels = sub.sample().expect("sample");
        let core = channels
            .iter()
            .find(|c| c.name.starts_with("ECPU") || c.name.starts_with("PCPU"))
            .expect("a core channel");
        let Value::State(states) = &core.value else {
            panic!("a state channel");
        };
        let ticks: i64 = states.iter().map(|(_, r)| r).sum();
        // Uptime in 24 MHz ticks: 125/3 ns each.
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: `ts` is a valid out-pointer.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut ts) },
            0
        );
        let ns = ts.tv_sec * 1_000_000_000 + ts.tv_nsec;
        let now = ns / 125 * 3;
        assert!(
            (ticks - now).abs() < now / 100,
            "residency is within 1% of uptime: {ticks} vs {now}"
        );
    }
}
