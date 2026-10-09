//! Minimal IOKit and CoreFoundation bindings for the macOS samplers that read
//! the I/O registry (`blockio`, `drivehealth`).
//!
//! Only what those samplers call is bound. Every owned registry object,
//! CoreFoundation object and plugin interface is wrapped so it is released on
//! drop. Struct layouts and UUIDs are from the macOS SDK headers named beside
//! each one.

use std::ffi::{c_char, c_void, CString};

type KernReturn = i32;
type IoObject = u32;
type CfTypeRef = *const c_void;
type CfAllocatorRef = *const c_void;

const KERN_SUCCESS: KernReturn = 0;
/// `kIOMainPortDefault`: the default main port, which is `MACH_PORT_NULL`.
const MAIN_PORT_DEFAULT: u32 = 0;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
/// `kCFNumberSInt64Type`; `CFNumberType` is a `CFIndex`.
const CF_NUMBER_SINT64_TYPE: isize = 4;
/// `kIORegistryIterateRecursively`.
const ITERATE_RECURSIVELY: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *const c_void,
        existing: *mut IoObject,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CfTypeRef,
        allocator: CfAllocatorRef,
        options: u32,
    ) -> CfTypeRef;
    fn IORegistryEntryGetRegistryEntryID(entry: IoObject, id: *mut u64) -> KernReturn;
    fn IORegistryEntrySearchCFProperty(
        entry: IoObject,
        plane: *const c_char,
        key: CfTypeRef,
        allocator: CfAllocatorRef,
        options: u32,
    ) -> CfTypeRef;
    fn IOCreatePlugInInterfaceForService(
        service: IoObject,
        plugin_type: CfTypeRef,
        interface_type: CfTypeRef,
        the_interface: *mut *mut *const IUnknownVtbl,
        the_score: *mut i32,
    ) -> KernReturn;
    fn IODestroyPlugInInterface(interface: *mut *const IUnknownVtbl) -> KernReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CfAllocatorRef,
        s: *const c_char,
        encoding: u32,
    ) -> CfTypeRef;
    fn CFDictionaryGetValue(dict: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn CFNumberGetValue(number: CfTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFGetTypeID(cf: CfTypeRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(boolean: CfTypeRef) -> u8;
    fn CFStringGetCString(s: CfTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFUUIDGetConstantUUIDWithBytes(
        alloc: CfAllocatorRef,
        b0: u8,
        b1: u8,
        b2: u8,
        b3: u8,
        b4: u8,
        b5: u8,
        b6: u8,
        b7: u8,
        b8: u8,
        b9: u8,
        b10: u8,
        b11: u8,
        b12: u8,
        b13: u8,
        b14: u8,
        b15: u8,
    ) -> CfTypeRef;
    fn CFRelease(cf: CfTypeRef);
}

/// The text of a CFString, if `v` is one.
///
/// # Safety
///
/// `v` must be null or a live CoreFoundation object.
unsafe fn string_value(v: CfTypeRef) -> Option<String> {
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    if CFStringGetCString(
        v,
        buf.as_mut_ptr(),
        buf.len() as isize,
        CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }
    Some(
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned(),
    )
}

/// An owned CoreFoundation object, released on drop.
struct Owned(CfTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `Owned` is only built from a non-null +1 reference returned
        // by a CoreFoundation or IOKit "Create" function.
        unsafe { CFRelease(self.0) }
    }
}

/// An immutable CoreFoundation string, for dictionary and property keys.
pub(crate) struct CfString(Owned);

// SAFETY: CFString is immutable and CoreFoundation's retain/release are
// thread-safe, so a key built once can be used from any thread.
unsafe impl Send for CfString {}
unsafe impl Sync for CfString {}

impl CfString {
    /// Build a key. Returns `None` if `s` contains a NUL byte or
    /// CoreFoundation cannot allocate.
    pub(crate) fn new(s: &str) -> Option<Self> {
        let c = CString::new(s).ok()?;
        // SAFETY: `c` is a valid NUL-terminated UTF-8 string for the call.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_STRING_ENCODING_UTF8)
        };
        (!r.is_null()).then(|| CfString(Owned(r)))
    }
}

/// An owned CoreFoundation dictionary read from the registry.
pub(crate) struct CfDictionary(Owned);

impl CfDictionary {
    /// The value at `key` as an `i64`, if it is present and a CFNumber.
    pub(crate) fn get_i64(&self, key: &CfString) -> Option<i64> {
        // SAFETY: `self.0` is a live CFDictionary and `key` a live CFString;
        // the returned value is borrowed from the dictionary (Get rule) and
        // only read while `self` is alive.
        unsafe {
            let v = CFDictionaryGetValue(self.0 .0, key.0 .0);
            if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
                return None;
            }
            let mut out: i64 = 0;
            (CFNumberGetValue(
                v,
                CF_NUMBER_SINT64_TYPE,
                &mut out as *mut i64 as *mut c_void,
            ) != 0)
                .then_some(out)
        }
    }

    /// The value at `key` as a `u64`, if it is present, a CFNumber, and not
    /// negative.
    pub(crate) fn get_u64(&self, key: &CfString) -> Option<u64> {
        self.get_i64(key).and_then(|v| u64::try_from(v).ok())
    }

    /// The value at `key` as a `String`, if it is present and a CFString of
    /// under 256 bytes.
    pub(crate) fn get_string(&self, key: &CfString) -> Option<String> {
        // SAFETY: as in `get_i64`; the value is borrowed from the dictionary.
        unsafe { string_value(CFDictionaryGetValue(self.0 .0, key.0 .0)) }
    }
}

/// An owned I/O registry entry, released on drop.
pub(crate) struct Service(IoObject);

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: `self.0` is an object this wrapper owns one reference to.
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

impl Service {
    /// Every registered service of IOKit class `class` (or a subclass).
    /// Returns an empty list if the lookup fails.
    pub(crate) fn matching(class: &str) -> Vec<Service> {
        let Ok(c) = CString::new(class) else {
            return Vec::new();
        };
        let mut iter: IoObject = 0;
        // SAFETY: `IOServiceMatching` returns a +1 dictionary (or null) that
        // `IOServiceGetMatchingServices` consumes, null included.
        let kr = unsafe {
            let matching = IOServiceMatching(c.as_ptr());
            IOServiceGetMatchingServices(MAIN_PORT_DEFAULT, matching, &mut iter)
        };
        if kr != KERN_SUCCESS || iter == 0 {
            return Vec::new();
        }
        let iter = Service(iter);
        let mut out = Vec::new();
        loop {
            // SAFETY: `iter.0` is a live iterator; each returned object is a
            // +1 reference that `Service` takes ownership of.
            let s = unsafe { IOIteratorNext(iter.0) };
            if s == 0 {
                break;
            }
            out.push(Service(s));
        }
        out
    }

    /// The registry entry ID, which is unique for the life of the entry and
    /// never reused while the system is up.
    pub(crate) fn id(&self) -> Option<u64> {
        let mut id = 0;
        // SAFETY: `self.0` is a live registry entry; `id` is a valid out-pointer.
        (unsafe { IORegistryEntryGetRegistryEntryID(self.0, &mut id) } == KERN_SUCCESS)
            .then_some(id)
    }

    /// The property `key` of this entry, if it is a dictionary.
    pub(crate) fn dictionary(&self, key: &CfString) -> Option<CfDictionary> {
        // SAFETY: `self.0` is a live registry entry and `key` a live CFString.
        // The result is a +1 reference (or null) that `Owned` takes over.
        let v = unsafe { IORegistryEntryCreateCFProperty(self.0, key.0 .0, std::ptr::null(), 0) };
        if v.is_null() {
            return None;
        }
        let v = Owned(v);
        // SAFETY: `v.0` is a live CF object.
        (unsafe { CFGetTypeID(v.0) == CFDictionaryGetTypeID() }).then(|| CfDictionary(v))
    }

    /// The property `key` of this entry, if it is a boolean.
    pub(crate) fn boolean(&self, key: &CfString) -> Option<bool> {
        // SAFETY: as in `dictionary`.
        let v = unsafe { IORegistryEntryCreateCFProperty(self.0, key.0 .0, std::ptr::null(), 0) };
        if v.is_null() {
            return None;
        }
        let v = Owned(v);
        // SAFETY: `v.0` is a live CF object.
        unsafe { (CFGetTypeID(v.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(v.0) != 0) }
    }

    /// The first string property `key` found on this entry or its children
    /// in the service plane, searched depth first.
    pub(crate) fn search_string(&self, key: &CfString) -> Option<String> {
        let plane = c"IOService";
        // SAFETY: `self.0` is a live registry entry, `plane` a NUL-terminated
        // plane name and `key` a live CFString. The result is +1 (or null).
        let v = unsafe {
            IORegistryEntrySearchCFProperty(
                self.0,
                plane.as_ptr(),
                key.0 .0,
                std::ptr::null(),
                ITERATE_RECURSIVELY,
            )
        };
        if v.is_null() {
            return None;
        }
        let v = Owned(v);
        // SAFETY: `v.0` is a live CF object.
        unsafe { string_value(v.0) }
    }
}

/// `CFUUIDBytes`, the by-value interface ID that `QueryInterface` takes.
#[repr(C)]
#[derive(Clone, Copy)]
struct CfUuidBytes([u8; 16]);

/// `IUNKNOWN_C_GUTS` (CoreFoundation `CFPlugInCOM.h`): the head of every
/// plugin interface's function table.
#[repr(C)]
struct IUnknownVtbl {
    _reserved: *const c_void,
    query_interface:
        unsafe extern "C" fn(this: *mut c_void, iid: CfUuidBytes, out: *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(this: *mut c_void) -> u32,
    release: unsafe extern "C" fn(this: *mut c_void) -> u32,
}

/// `IONVMeSMARTInterface` (IOKit `storage/nvme/NVMeSMARTLibExternal.h`), up
/// to the last entry this module calls.
#[repr(C)]
struct NvmeSmartVtbl {
    base: IUnknownVtbl,
    version: u16,
    revision: u16,
    smart_read_data: unsafe extern "C" fn(this: *mut c_void, data: *mut c_void) -> i32,
    get_identify_data:
        unsafe extern "C" fn(this: *mut c_void, data: *mut c_void, namespace: u32) -> i32,
    _reserved0: u64,
    _reserved1: u64,
    get_log_page: unsafe extern "C" fn(
        this: *mut c_void,
        data: *mut c_void,
        log_page_id: u32,
        num_dwords: u32,
    ) -> i32,
}

/// `kIOCFPlugInInterfaceID` (IOKit `IOCFPlugIn.h`).
const CF_PLUGIN_INTERFACE_ID: [u8; 16] = [
    0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F,
];
/// `kIONVMeSMARTUserClientTypeID` (`NVMeSMARTLibExternal.h`).
const NVME_SMART_USER_CLIENT_TYPE_ID: [u8; 16] = [
    0xAA, 0x0F, 0xA6, 0xF9, 0xC2, 0xD6, 0x45, 0x7F, 0xB1, 0x0B, 0x59, 0xA1, 0x32, 0x53, 0x29, 0x2F,
];
/// `kIONVMeSMARTInterfaceID` (`NVMeSMARTLibExternal.h`).
const NVME_SMART_INTERFACE_ID: [u8; 16] = [
    0xCC, 0xD1, 0xDB, 0x19, 0xFD, 0x9A, 0x4D, 0xAF, 0xBF, 0x95, 0x12, 0x45, 0x4B, 0x23, 0x0A, 0xB6,
];

fn constant_uuid(b: [u8; 16]) -> CfTypeRef {
    // SAFETY: returns a constant UUID object owned by CoreFoundation; it is
    // never released.
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15],
        )
    }
}

/// An open NVMe SMART user client on one device, released on drop.
pub(crate) struct NvmeSmart {
    /// The plugin interface. It must be destroyed with
    /// `IODestroyPlugInInterface`, and after `this` is released: destroying it
    /// first closes the connection, so `GetLogPage` fails (`0x10000003`), and
    /// releasing it with `Release` instead leaks the user client, so every
    /// later open from this process fails with `kIOReturnNoResources`.
    plugin: *mut *const IUnknownVtbl,
    this: *mut *const NvmeSmartVtbl,
}

impl NvmeSmart {
    /// Open the SMART interface of `service`, a device whose
    /// `NVMe SMART Capable` property is true. A device has one such client at
    /// a time: this returns `None` while any process, this one included,
    /// holds it open.
    pub(crate) fn open(service: &Service) -> Option<Self> {
        let mut plugin: *mut *const IUnknownVtbl = std::ptr::null_mut();
        let mut score = 0;
        // SAFETY: valid service, constant UUIDs and out-pointers. On success
        // `plugin` is a +1 interface pointer.
        let kr = unsafe {
            IOCreatePlugInInterfaceForService(
                service.0,
                constant_uuid(NVME_SMART_USER_CLIENT_TYPE_ID),
                constant_uuid(CF_PLUGIN_INTERFACE_ID),
                &mut plugin,
                &mut score,
            )
        };
        if kr != KERN_SUCCESS || plugin.is_null() {
            return None;
        }
        let mut smart: *mut c_void = std::ptr::null_mut();
        // SAFETY: `plugin` is a live interface; QueryInterface returns a +1
        // reference to the SMART interface on success.
        let hr = unsafe {
            ((**plugin).query_interface)(
                plugin as *mut c_void,
                CfUuidBytes(NVME_SMART_INTERFACE_ID),
                &mut smart,
            )
        };
        if hr != 0 || smart.is_null() {
            // SAFETY: `plugin` is the interface created above, destroyed once.
            unsafe { IODestroyPlugInInterface(plugin) };
            return None;
        }
        Some(Self {
            plugin,
            this: smart as *mut *const NvmeSmartVtbl,
        })
    }

    /// The 512-byte SMART / Health Information log page (0x02).
    pub(crate) fn health_log(&self) -> Option<[u8; 512]> {
        let mut buf = [0u8; 512];
        // SAFETY: `self.this` is a live SMART interface and `buf` holds the
        // 128 dwords requested (the count is zero based).
        let kr = unsafe {
            ((**self.this).get_log_page)(
                self.this as *mut c_void,
                buf.as_mut_ptr() as *mut c_void,
                0x02,
                127,
            )
        };
        (kr == KERN_SUCCESS).then_some(buf)
    }
}

impl Drop for NvmeSmart {
    fn drop(&mut self) {
        // SAFETY: `self.this` holds one reference and `self.plugin` was
        // created by `open`; each is released once, interface first.
        unsafe {
            ((**self.this).base.release)(self.this as *mut c_void);
            IODestroyPlugInInterface(self.plugin);
        }
    }
}

/// Held by every test that opens an NVMe SMART interface, since a device
/// accepts one open at a time and tests run in parallel.
#[cfg(test)]
pub(crate) static NVME_SMART_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens, reads and drops each SMART-capable device's interface three
    /// times, printing the page (run with `--nocapture` to see it). A second
    /// open fails with `kIOReturnNoResources` if a dropped interface leaked
    /// its user client, so the repeat is the point of the test.
    #[test]
    fn nvme_smart_health_log_can_be_read_repeatedly() {
        let _lock = NVME_SMART_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let capable = CfString::new("NVMe SMART Capable").unwrap();
        let devices: Vec<Service> = Service::matching("IOBlockStorageDevice")
            .into_iter()
            .filter(|s| s.boolean(&capable) == Some(true))
            .collect();
        if devices.is_empty() {
            eprintln!("no NVMe SMART capable device on this host; skipping");
            return;
        }
        for round in 0..3 {
            for s in &devices {
                let smart = NvmeSmart::open(s)
                    .unwrap_or_else(|| panic!("round {round}: open the NVMe SMART interface"));
                let page = smart.health_log().expect("read log page 0x02");
                let kelvin = u16::from_le_bytes([page[1], page[2]]);
                let u32le = |o: usize| u32::from_le_bytes(page[o..o + 4].try_into().unwrap());
                println!(
                    "temperature {} K, warning time {} min, critical time {} min, tmt {:?} {:?}",
                    kelvin,
                    u32le(192),
                    u32le(196),
                    [u32le(216), u32le(220)],
                    [u32le(224), u32le(228)],
                );
                assert!(kelvin > 273, "temperature field not reported");
            }
        }
    }
}
