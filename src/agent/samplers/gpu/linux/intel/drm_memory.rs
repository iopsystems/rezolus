//! Device-level VRAM accounting via `DRM_IOCTL_I915_QUERY`
//! (`DRM_I915_QUERY_MEMORY_REGIONS`).
//!
//! The i915 PMU exposes no memory counter, so VRAM usage comes from the DRM
//! query ioctl instead. It reports, per memory region, the size the driver
//! probed and an estimate of how much is still unallocated — which is exactly
//! the `total`/`free` pair the AMD and NVIDIA samplers publish as
//! `gpu_memory{state=used,free}`.
//!
//! ## Which region counts
//!
//! A region is classed `SYSTEM` (host memory) or `DEVICE` (card-local VRAM).
//! Only `DEVICE` is VRAM, and only `DEVICE` has real allocation tracking — the
//! uapi states `unallocated_size` is "only currently tracked for
//! I915_MEMORY_CLASS_DEVICE regions". An integrated GPU has no `DEVICE` region
//! at all (verified on a Coffee Lake iGPU, which reports a single `SYSTEM`
//! region), so it correctly publishes no VRAM series rather than reporting host
//! RAM as if it were VRAM.
//!
//! ## The privilege trap
//!
//! `unallocated_size` needs `CAP_PERFMON` (or `CAP_SYS_ADMIN`). Without it the
//! kernel does **not** fail the ioctl — it silently returns
//! `unallocated_size == probed_size`, which reads as a perfectly plausible
//! "0 bytes used". Verified on an Arc A770: as root the query reported 14366 MiB
//! used of 16288 MiB, and unprivileged the same query reported 0 used.
//!
//! Publishing that zero would be worse than publishing nothing, so
//! [`MemoryRegions::vram`] treats `unallocated == probed` on a `DEVICE` region as
//! "not accounted" and returns `None`. Rezolus already holds `CAP_PERFMON` for
//! the PMU, so in normal operation the values are real.
//!
//! ## Cost
//!
//! Measured at ~5.4 us per call on both an Arc A770 and an integrated GPU —
//! comparable to a single perf counter read. It is still an ioctl rather than an
//! mmap load, so it is subject to the same throttling as the PMU reads.

use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

/// `DRM_IOCTL_I915_QUERY`, from `DRM_IOWR(DRM_COMMAND_BASE + DRM_I915_QUERY, ...)`.
///
/// `DRM_COMMAND_BASE` is 0x40 and `DRM_I915_QUERY` is 0x39, giving ioctl number
/// 0x79 in the 'd' (0x64) group with a 16-byte `drm_i915_query` argument.
const DRM_IOCTL_I915_QUERY: libc::c_ulong = 0xc0106479;

/// `DRM_I915_QUERY_MEMORY_REGIONS`
const QUERY_MEMORY_REGIONS: u64 = 4;

/// `I915_MEMORY_CLASS_DEVICE` — card-local memory (VRAM).
const MEMORY_CLASS_DEVICE: u16 = 1;

/// `struct drm_i915_query`
#[repr(C)]
struct DrmI915Query {
    num_items: u32,
    flags: u32,
    items_ptr: u64,
}

/// `struct drm_i915_query_item`
#[repr(C)]
struct DrmI915QueryItem {
    query_id: u64,
    /// Set to 0 to ask for the required length; the kernel writes the byte
    /// count here. A negative value is an error code.
    length: i32,
    flags: u32,
    data_ptr: u64,
}

/// `struct drm_i915_gem_memory_class_instance`
#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryClassInstance {
    memory_class: u16,
    memory_instance: u16,
}

/// `struct drm_i915_memory_region_info`
///
/// The trailing union is 8 x `__u64`; we only read the first two members of its
/// struct arm (`probed_cpu_visible_size`, `unallocated_cpu_visible_size`), which
/// we do not currently publish, so the tail is kept as an opaque array.
#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryRegionInfo {
    region: MemoryClassInstance,
    rsvd0: u32,
    probed_size: u64,
    unallocated_size: u64,
    rsvd1: [u64; 8],
}

/// `struct drm_i915_query_memory_regions` header (the `regions[]` flexible array
/// follows it in the same allocation).
#[repr(C)]
struct QueryMemoryRegionsHeader {
    num_regions: u32,
    rsvd: [u32; 3],
}

/// Total and free bytes for a GPU's device-local memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vram {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Vram {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

/// An open DRM render node used to query memory regions.
pub struct DrmDevice {
    file: File,
    label: String,
}

impl DrmDevice {
    /// Open the render node whose PCI address matches `pci_address`.
    ///
    /// Render nodes (`renderD*`) are used rather than the primary node
    /// (`card*`): they need no DRM master and are the interface intended for
    /// non-display clients.
    pub fn for_pci_address(pci_address: &str) -> Option<Self> {
        let entries = std::fs::read_dir("/sys/class/drm").ok()?;

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("renderD") {
                continue;
            }

            // /sys/class/drm/renderD129/device resolves to the PCI device dir,
            // whose basename is the BDF (e.g. 0000:04:00.0).
            let device_link = entry.path().join("device");
            let Ok(target) = std::fs::canonicalize(&device_link) else {
                continue;
            };
            let Some(bdf) = target.file_name().and_then(|s| s.to_str()) else {
                continue;
            };

            if bdf == pci_address {
                let path = Path::new("/dev/dri").join(&name);
                let file = File::open(&path).ok()?;
                return Some(Self { file, label: name });
            }
        }

        None
    }

    fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// The render node name, for diagnostics.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Query device-local (VRAM) total and free bytes.
    ///
    /// Returns `None` when the GPU has no device-local memory (an integrated
    /// GPU), when the ioctl is unavailable, or when allocation accounting is not
    /// available to this process (see the privilege note in the module docs).
    pub fn vram(&self) -> Option<Vram> {
        let regions = self.query_memory_regions()?;

        regions.iter().find_map(|region| {
            if region.region.memory_class != MEMORY_CLASS_DEVICE {
                return None;
            }

            // Without CAP_PERFMON the kernel reports everything as unallocated
            // rather than failing. Publishing "0 used" would be a confident lie,
            // so report nothing instead.
            if region.unallocated_size >= region.probed_size {
                return None;
            }

            Some(Vram {
                total_bytes: region.probed_size,
                free_bytes: region.unallocated_size,
            })
        })
    }

    /// Issue the two-step query (ask for length, then fetch) and return the
    /// region array.
    fn query_memory_regions(&self) -> Option<Vec<MemoryRegionInfo>> {
        // Step 1: length probe. `length = 0` asks the kernel how many bytes the
        // reply needs.
        let mut item = DrmI915QueryItem {
            query_id: QUERY_MEMORY_REGIONS,
            length: 0,
            flags: 0,
            data_ptr: 0,
        };

        if !self.query(&mut item) || item.length <= 0 {
            return None;
        }

        let length = item.length as usize;

        let header_size = std::mem::size_of::<QueryMemoryRegionsHeader>();
        let region_size = std::mem::size_of::<MemoryRegionInfo>();
        if length < header_size {
            return None;
        }

        // Step 2: fetch into a buffer the kernel sized for us. The buffer is
        // `u64`-aligned because both structs are 8-byte aligned.
        let words = length.div_ceil(std::mem::size_of::<u64>());
        let mut buffer: Vec<u64> = vec![0; words];

        item.data_ptr = buffer.as_mut_ptr() as u64;
        if !self.query(&mut item) {
            return None;
        }

        // SAFETY: `buffer` is at least `length` bytes, 8-byte aligned, and the
        // kernel has just written a `drm_i915_query_memory_regions` into it.
        let header = unsafe { &*(buffer.as_ptr() as *const QueryMemoryRegionsHeader) };
        let num_regions = header.num_regions as usize;

        // Trust the declared length over `num_regions`: only read as many
        // regions as actually fit in the buffer the kernel filled.
        let available = (length - header_size) / region_size;
        let count = num_regions.min(available);

        let mut regions = Vec::with_capacity(count);
        for i in 0..count {
            let offset = header_size + i * region_size;
            // SAFETY: `offset + region_size <= length <= buffer bytes`, and the
            // region array directly follows the header per the uapi layout.
            let region = unsafe {
                std::ptr::read_unaligned(
                    (buffer.as_ptr() as *const u8).add(offset) as *const MemoryRegionInfo
                )
            };
            regions.push(region);
        }

        Some(regions)
    }

    fn query(&self, item: &mut DrmI915QueryItem) -> bool {
        let mut query = DrmI915Query {
            num_items: 1,
            flags: 0,
            items_ptr: item as *mut DrmI915QueryItem as u64,
        };

        // SAFETY: `query` points at a valid `drm_i915_query` whose `items_ptr`
        // refers to a single valid `drm_i915_query_item`, matching what
        // DRM_IOCTL_I915_QUERY expects.
        let ret = unsafe {
            libc::ioctl(
                self.fd(),
                DRM_IOCTL_I915_QUERY,
                &mut query as *mut DrmI915Query,
            )
        };

        ret == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_the_uapi() {
        // These sizes are fixed by the kernel uapi; a mismatch would silently
        // misparse the reply.
        assert_eq!(std::mem::size_of::<DrmI915Query>(), 16);
        assert_eq!(std::mem::size_of::<DrmI915QueryItem>(), 24);
        assert_eq!(std::mem::size_of::<MemoryClassInstance>(), 4);
        assert_eq!(std::mem::size_of::<QueryMemoryRegionsHeader>(), 16);
        // 4 (class:instance) + 4 (rsvd0) + 8 (probed) + 8 (unallocated)
        // + 64 (union) = 88
        assert_eq!(std::mem::size_of::<MemoryRegionInfo>(), 88);
    }

    #[test]
    fn used_is_total_minus_free() {
        let v = Vram {
            total_bytes: 16 * 1024 * 1024 * 1024,
            free_bytes: 2 * 1024 * 1024 * 1024,
        };
        assert_eq!(v.used_bytes(), 14 * 1024 * 1024 * 1024);
    }

    #[test]
    fn used_saturates_rather_than_underflowing() {
        // Defensive: the kernel should never report free > total, but the
        // subtraction must not wrap if it does.
        let v = Vram {
            total_bytes: 1024,
            free_bytes: 4096,
        };
        assert_eq!(v.used_bytes(), 0);
    }
}
