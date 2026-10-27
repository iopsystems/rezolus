//! Memory samplers on macOS, from `host_statistics64(HOST_VM_INFO64)` (the
//! counts `vm_stat` prints) and the `hw.memsize` and `vm.swapusage` sysctls.
//!
//! They use the Linux samplers' names, `memory_meminfo` for the gauges and
//! `memory_vmstat` for the cumulative counters, and set the Linux metrics that
//! have a macOS equivalent. Metrics with no equivalent are left unset, and an
//! unset lazy metric is absent from snapshots. `memory_active` and
//! `memory_inactive` are among them: Linux splits those lists by `kind=anon`
//! and `kind=file`, and macOS does not. Counts with no Linux equivalent, most
//! of them about the compressor, are macOS-only metrics in each sampler's
//! `macos_stats`.

mod meminfo;
mod vmstat;

use std::ffi::CStr;

/// The VM statistics, or `None` if the call fails or the kernel's answer
/// stops before the compressor and file-backed/anonymous counts (revision 1
/// of `vm_statistics64`, macOS 10.9).
fn vm_statistics(host: libc::mach_port_t) -> Option<libc::vm_statistics64> {
    // SAFETY: `vm_statistics64` is plain integers; all-zero is a valid value.
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;

    // SAFETY: `count` is the length of `stats` in `integer_t`s; the kernel
    // writes at most that many and returns how many it wrote.
    let rc = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            &mut stats as *mut _ as libc::host_info64_t,
            &mut count,
        )
    };

    let rev1 = std::mem::offset_of!(libc::vm_statistics64, swapped_count)
        / std::mem::size_of::<libc::integer_t>();

    (rc == libc::KERN_SUCCESS && count as usize >= rev1).then_some(stats)
}

/// A fixed-size sysctl value, or `None` if the read fails or returns a
/// different size.
fn sysctl<T: Copy>(name: &CStr) -> Option<T> {
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut len = std::mem::size_of::<T>();

    // SAFETY: the kernel writes at most `len` bytes into `value`.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };

    // SAFETY: a successful read of exactly `size_of::<T>()` bytes initialised
    // `value`.
    (rc == 0 && len == std::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

/// The kernel's page size, which `vm_statistics64` counts in.
fn page_size() -> Option<u64> {
    sysctl::<libc::c_int>(c"hw.pagesize").and_then(|n| u64::try_from(n).ok())
}

#[allow(deprecated)]
fn host() -> libc::mach_port_t {
    // SAFETY: no preconditions; returns this task's host port.
    unsafe { libc::mach_host_self() }
}
