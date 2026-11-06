//! The IOHIDEventSystem services that publish the SoC's power-manager (PMU),
//! battery and NAND sensors.
//!
//! `IOHIDEventSystemClient` is private API: IOKit.framework exports these
//! functions but the SDK has no header for them, so the signatures below are
//! the ones open-source tools such as `macmon` and `Stats` declare. A service
//! is matched by its primary usage page and usage. Its `Product` is a name
//! such as `PMU tdie1`, and its `LocationID` is a four-character code such as
//! `TP1b`. For a voltage or current service the code is the name of the SMC
//! key for the same rail, when the SMC has that key.

use crate::agent::samplers::iokit::{string_value, CfString};
use std::ffi::c_void;

type CfTypeRef = *const c_void;

/// The vendor usage page and usage of the temperature services.
pub(crate) const TEMPERATURE: (i32, i32) = (0xff00, 5);
/// The vendor usage page and usage of the current services.
pub(crate) const CURRENT: (i32, i32) = (0xff08, 2);
/// The vendor usage page and usage of the voltage services.
pub(crate) const VOLTAGE: (i32, i32) = (0xff08, 3);

/// `kIOHIDEventTypeTemperature`.
const EVENT_TYPE_TEMPERATURE: i64 = 15;
/// `kCFNumberSInt32Type`.
const CF_NUMBER_SINT32_TYPE: isize = 3;
/// `kCFNumberSInt64Type`.
const CF_NUMBER_SINT64_TYPE: isize = 4;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: CfTypeRef) -> CfTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CfTypeRef, matching: CfTypeRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: CfTypeRef) -> CfTypeRef;
    fn IOHIDServiceClientCopyProperty(service: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn IOHIDServiceClientCopyEvent(
        service: CfTypeRef,
        kind: i64,
        options: i32,
        timestamp: i64,
    ) -> CfTypeRef;
    fn IOHIDEventGetFloatValue(event: CfTypeRef, field: i32) -> f64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        allocator: CfTypeRef,
        keys: *const CfTypeRef,
        values: *const CfTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CfTypeRef;
    fn CFNumberCreate(allocator: CfTypeRef, kind: isize, value: *const c_void) -> CfTypeRef;
    fn CFNumberGetValue(number: CfTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFGetTypeID(cf: CfTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFArrayGetCount(array: CfTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: isize) -> CfTypeRef;
    fn CFRelease(cf: CfTypeRef);
}

/// An owned CoreFoundation object, released on drop.
struct Owned(CfTypeRef);

impl Owned {
    /// Take ownership of `r`, a +1 reference, unless it is null.
    fn new(r: CfTypeRef) -> Option<Self> {
        // Not `then_some`: that builds the `Owned` first and drops it, so it
        // would release a null pointer.
        (!r.is_null()).then(|| Owned(r))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `Owned` holds a non-null +1 reference, released once.
        unsafe { CFRelease(self.0) }
    }
}

fn number(v: i32) -> Option<Owned> {
    // SAFETY: `v` is a valid `SInt32` for the call; the result is +1 or null.
    Owned::new(unsafe {
        CFNumberCreate(
            std::ptr::null(),
            CF_NUMBER_SINT32_TYPE,
            &v as *const i32 as *const c_void,
        )
    })
}

/// One service: its index in the client's service list, `Product` and
/// `LocationID`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HidService {
    pub index: usize,
    pub product: String,
    pub location: u32,
}

/// The services of one usage page and usage, held open for reading.
pub(crate) struct Services {
    // Field order is drop order: the service list before its client.
    services: Owned,
    _client: Owned,
}

// SAFETY: the client and its services are CoreFoundation objects with
// thread-safe reference counts. The client is created without a run loop or
// dispatch queue, and `IOHIDServiceClientCopyEvent` is a synchronous call
// that may be made from any thread; the live test reads from a second
// thread. `Services` is not `Sync`.
unsafe impl Send for Services {}

impl Services {
    /// The services matching `(page, usage)`, and their names and locations.
    pub(crate) fn open((page, usage): (i32, i32)) -> Option<(Self, Vec<HidService>)> {
        let page_key = CfString::new("PrimaryUsagePage")?;
        let usage_key = CfString::new("PrimaryUsage")?;
        let page = number(page)?;
        let usage = number(usage)?;
        let keys = [page_key.as_ptr(), usage_key.as_ptr()];
        let values = [page.0, usage.0];
        // SAFETY: two live keys and values; the CFType callbacks retain them.
        // The results are +1 references (or null) that `Owned` takes over.
        let (client, services) = unsafe {
            let matching = Owned::new(CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            ))?;
            let client = Owned::new(IOHIDEventSystemClientCreate(std::ptr::null()))?;
            IOHIDEventSystemClientSetMatching(client.0, matching.0);
            let services = Owned::new(IOHIDEventSystemClientCopyServices(client.0))?;
            (client, services)
        };
        let out = Services {
            services,
            _client: client,
        };
        let product_key = CfString::new("Product")?;
        let location_key = CfString::new("LocationID")?;
        let list = (0..out.len())
            .filter_map(|index| {
                let service = out.service(index)?;
                Some(HidService {
                    index,
                    product: property_string(service, &product_key).unwrap_or_default(),
                    location: property_u32(service, &location_key)?,
                })
            })
            .collect();
        Some((out, list))
    }

    fn len(&self) -> usize {
        // SAFETY: `self.services` is a live CFArray.
        usize::try_from(unsafe { CFArrayGetCount(self.services.0) }).unwrap_or(0)
    }

    /// The service at `index`, borrowed from the list.
    fn service(&self, index: usize) -> Option<CfTypeRef> {
        if index >= self.len() {
            return None;
        }
        // SAFETY: `index` is in bounds; the value is borrowed (Get rule) and
        // used only while `self` holds the array.
        let s = unsafe { CFArrayGetValueAtIndex(self.services.0, index as isize) };
        (!s.is_null()).then_some(s)
    }

    /// The temperature the service at `index` reports, in degrees Celsius.
    pub(crate) fn temperature(&self, index: usize) -> Option<f64> {
        let service = self.service(index)?;
        // SAFETY: `service` is a live service client; the event is +1 or null.
        let event = Owned::new(unsafe {
            IOHIDServiceClientCopyEvent(service, EVENT_TYPE_TEMPERATURE, 0, 0)
        })?;
        // The temperature level is field 0 of the event type, whose fields
        // are numbered from `type << 16`.
        // SAFETY: `event.0` is a live temperature event.
        Some(unsafe { IOHIDEventGetFloatValue(event.0, (EVENT_TYPE_TEMPERATURE as i32) << 16) })
    }
}

fn property(service: CfTypeRef, key: &CfString) -> Option<Owned> {
    // SAFETY: `service` is a live service client and `key` a live CFString;
    // the result is +1 or null.
    Owned::new(unsafe { IOHIDServiceClientCopyProperty(service, key.as_ptr()) })
}

fn property_string(service: CfTypeRef, key: &CfString) -> Option<String> {
    let v = property(service, key)?;
    // SAFETY: `v.0` is a live CF object.
    unsafe { string_value(v.0) }
}

fn property_u32(service: CfTypeRef, key: &CfString) -> Option<u32> {
    let v = property(service, key)?;
    let mut out: i64 = 0;
    // SAFETY: `v.0` is a live CF object and `out` a valid out-pointer.
    let ok = unsafe {
        CFGetTypeID(v.0) == CFNumberGetTypeID()
            && CFNumberGetValue(
                v.0,
                CF_NUMBER_SINT64_TYPE,
                &mut out as *mut i64 as *mut c_void,
            ) != 0
    };
    ok.then(|| u32::try_from(out).ok()).flatten()
}
