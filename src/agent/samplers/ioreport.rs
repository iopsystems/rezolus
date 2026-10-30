//! Bindings for IOReport, the macOS framework `powermetrics` reads its
//! energy and CPU performance-state counters from, for the `cpu_power` and
//! `cpu_frequency` samplers.
//!
//! IOReport is private: it ships in the shared cache as `libIOReport.dylib`
//! with no header, and Apple documents none of it. The signatures here are
//! the ones open-source tools that read it use, and only what the two
//! samplers call is bound. It needs no privileges. A subscription's samples
//! are cumulative since boot: a simple channel's value is a running total,
//! and a state channel's residencies are running totals of 24 MHz system
//! counter ticks.

use std::ffi::{c_char, c_void, CString};

type CfTypeRef = *const c_void;

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// `IOReportChannelGetFormat`'s value for a channel with one integer.
const FORMAT_SIMPLE: i32 = 1;
/// `IOReportChannelGetFormat`'s value for a channel with a residency per
/// state.
const FORMAT_STATE: i32 = 2;

#[link(name = "IOReport")]
extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CfTypeRef,
        subgroup: CfTypeRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CfTypeRef;
    fn IOReportMergeChannels(a: CfTypeRef, b: CfTypeRef, null: CfTypeRef);
    fn IOReportCreateSubscription(
        a: *const c_void,
        desired: CfTypeRef,
        subscribed: *mut CfTypeRef,
        channel_id: u64,
        b: CfTypeRef,
    ) -> CfTypeRef;
    fn IOReportCreateSamples(
        subscription: CfTypeRef,
        subscribed: CfTypeRef,
        null: CfTypeRef,
    ) -> CfTypeRef;
    fn IOReportChannelGetGroup(item: CfTypeRef) -> CfTypeRef;
    fn IOReportChannelGetSubGroup(item: CfTypeRef) -> CfTypeRef;
    fn IOReportChannelGetChannelName(item: CfTypeRef) -> CfTypeRef;
    fn IOReportChannelGetUnitLabel(item: CfTypeRef) -> CfTypeRef;
    fn IOReportChannelGetFormat(item: CfTypeRef) -> i32;
    fn IOReportSimpleGetIntegerValue(item: CfTypeRef, index: *mut i32) -> i64;
    fn IOReportStateGetCount(item: CfTypeRef) -> i32;
    fn IOReportStateGetNameForIndex(item: CfTypeRef, index: i32) -> CfTypeRef;
    fn IOReportStateGetResidency(item: CfTypeRef, index: i32) -> i64;
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
    fn CFRelease(cf: CfTypeRef);
}

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
    pub(crate) fn new(groups: &[(&str, Option<&str>)]) -> Option<Self> {
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
                IOReportCopyChannelsInGroup(
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
            let channels = Owned(channels);
            match &merged {
                // SAFETY: both are live channel dictionaries; the first is
                // a mutable copy, which the merge adds to.
                Some(m) => unsafe { IOReportMergeChannels(m.0, channels.0, std::ptr::null()) },
                None => {
                    // SAFETY: `channels.0` is a live dictionary; the copy is +1.
                    let copy = unsafe {
                        CFDictionaryCreateMutableCopy(
                            std::ptr::null(),
                            CFDictionaryGetCount(channels.0),
                            channels.0,
                        )
                    };
                    if copy.is_null() {
                        return None;
                    }
                    merged = Some(Owned(copy));
                }
            }
        }
        let desired = merged?;
        let mut subscribed: CfTypeRef = std::ptr::null();
        // SAFETY: `desired` is a live mutable channel dictionary and
        // `subscribed` a valid out-pointer; both results are +1 or null.
        let subscription = unsafe {
            IOReportCreateSubscription(
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
            subscription: Owned(subscription),
            subscribed: Owned(subscribed),
        })
    }

    /// Every subscribed channel's running totals. Simple and state channels
    /// are returned; other formats are left out.
    pub(crate) fn sample(&self) -> Option<Vec<Channel>> {
        // SAFETY: both objects are live; the result is +1 or null.
        let samples = unsafe {
            IOReportCreateSamples(self.subscription.0, self.subscribed.0, std::ptr::null())
        };
        if samples.is_null() {
            return None;
        }
        let samples = Owned(samples);
        let key = cf_string("IOReportChannels")?;
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
                let value = match IOReportChannelGetFormat(item) {
                    FORMAT_SIMPLE => {
                        Value::Simple(IOReportSimpleGetIntegerValue(item, std::ptr::null_mut()))
                    }
                    FORMAT_STATE => Value::State(
                        (0..IOReportStateGetCount(item))
                            .map(|s| {
                                (
                                    text(IOReportStateGetNameForIndex(item, s)),
                                    IOReportStateGetResidency(item, s),
                                )
                            })
                            .collect(),
                    ),
                    _ => continue,
                };
                out.push(Channel {
                    group: text(IOReportChannelGetGroup(item)),
                    subgroup: text(IOReportChannelGetSubGroup(item)),
                    name: text(IOReportChannelGetChannelName(item)),
                    unit: text(IOReportChannelGetUnitLabel(item)),
                    value,
                });
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The energy model is readable unprivileged and its counters only grow.
    #[test]
    fn energy_model_totals_are_running_totals() {
        let sub = Subscription::new(&[("Energy Model", None)]).expect("subscribe");
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
        let sub = Subscription::new(&[("CPU Stats", Some("CPU Core Performance States"))])
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
