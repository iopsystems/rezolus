//! Memory samplers on macOS.
//!
//! They use the Linux samplers' names, `memory_meminfo` for the gauges and
//! `memory_vmstat` for the cumulative counters, and set the Linux metrics that
//! have a macOS equivalent. Metrics with no equivalent are left unset, and an
//! unset lazy metric is absent from snapshots. `memory_active` and
//! `memory_inactive` are among them: Linux splits those lists by `kind=anon`
//! and `kind=file`, and macOS does not. Counts with no Linux equivalent
//! (wired, purgeable and speculative pages, the compressor, file page-ins and
//! page-outs) are macOS-only metrics in each sampler's `macos_stats`.
//!
//! The values come from `vm.*` sysctls where one exists, and otherwise from
//! `host_statistics64(HOST_VM_INFO64)`, the call `vm_stat` makes. That call is
//! rate-limited for every process that is not an Apple platform binary: all
//! such processes share one budget of 2 to 10 calls per second, and a call
//! over it returns the last result any of them fetched, with `KERN_SUCCESS`.
//! Only the values with no sysctl (faults, page-ins and page-outs,
//! compressions and decompressions, wired pages) are read that way, once per
//! refresh, by `memory_vmstat`.

mod meminfo;
mod vmstat;

use std::ffi::CStr;

/// The VM statistics, or `None` if the call fails or the kernel's answer
/// stops before revision 1 of `vm_statistics64` (which added the compressor
/// counters). The result may be cached; see the module doc.
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

/// An unsigned integer sysctl of 4 or 8 bytes, or `None` if the read fails
/// or the value has another size.
fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();

    // SAFETY: the kernel writes at most `len` bytes into `buf`.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };

    match (rc, len) {
        (0, 4) => Some(u32::from_ne_bytes(buf[..4].try_into().unwrap()) as u64),
        (0, 8) => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// Swap usage, or `None` if the read fails.
fn swap_usage() -> Option<libc::xsw_usage> {
    let mut value = std::mem::MaybeUninit::<libc::xsw_usage>::uninit();
    let mut len = std::mem::size_of::<libc::xsw_usage>();

    // SAFETY: the kernel writes at most `len` bytes into `value`.
    let rc = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            value.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };

    // SAFETY: a successful read of the whole struct initialised `value`.
    (rc == 0 && len == std::mem::size_of::<libc::xsw_usage>())
        .then(|| unsafe { value.assume_init() })
}

/// The kernel's page size, which the page counts are in. `hw.pagesize` is the
/// calling process's page size and reads 4096 for an x86_64 build under
/// Rosetta on a 16 KiB kernel; `vm.pagesize` is the kernel's.
fn page_size() -> Option<u64> {
    sysctl_u64(c"vm.pagesize").filter(|n| *n > 0)
}
