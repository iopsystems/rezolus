//! GPU hardware inventory.
//!
//! Describes the GPUs present on the system: identity (name, vendor, total
//! memory, driver), topology (PCI bus id, NUMA node), and capabilities
//! (architecture / compute capability, PCIe gen & width, core/SM counts) where
//! the vendor library exposes them.
//!
//! The vendor libraries are loaded at runtime with `dlopen` (via `libloading`)
//! rather than linked at build time — `libnvidia-ml.so` for NVIDIA and
//! `librocm_smi64.so` for AMD — so `systeminfo` builds on hosts without CUDA or
//! ROCm installed. When neither library (or no GPU) is present, `get_gpus()`
//! simply returns an empty vector. This mirrors how the agent's GPU samplers
//! load these libraries.
//!
//! Intel needs no vendor library: the i915/xe driver publishes everything this
//! inventory wants through sysfs and one DRM ioctl, which is also what the
//! `gpu_intel_pmu` sampler reads. Both integrated and discrete (Arc) parts are
//! enumerated.
//!
//! This is *static* hardware inventory only. Live telemetry (utilization,
//! temperature, power, clocks) belongs in the agent's GPU samplers, not here.

#![allow(non_camel_case_types)]

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gpu {
    /// Device index as reported by the vendor library.
    pub index: usize,
    /// Vendor identifier: "nvidia", "amd" or "intel".
    pub vendor: String,
    /// Marketing/device name, e.g. "NVIDIA A100-SXM4-80GB" or
    /// "AMD Radeon AI PRO R9700".
    pub name: Option<String>,
    /// Total video memory in bytes.
    pub memory_bytes: Option<u64>,
    /// Driver version string.
    pub driver: Option<String>,
    /// PCI bus identifier, e.g. "0000:c1:00.0".
    pub pci_bus_id: Option<String>,
    /// NUMA node the GPU is attached to, if known.
    pub numa_node: Option<usize>,
    /// Architecture / compute capability:
    /// - NVIDIA: compute capability, e.g. "8.0".
    /// - AMD: LLVM target / gfx name, e.g. "gfx942".
    /// - Intel: "integrated" or "discrete".
    pub architecture: Option<String>,
    /// Current PCIe link generation (1-7).
    pub pcie_gen: Option<usize>,
    /// Current PCIe link width (number of lanes).
    pub pcie_width: Option<usize>,
    /// Number of compute cores:
    /// - NVIDIA: streaming multiprocessor (SM) count.
    /// - AMD: compute unit (CU) count.
    /// - Intel: not reported (the driver exposes no EU count in sysfs).
    pub cores: Option<usize>,
}

/// Discover all GPUs on the system, querying every available vendor backend.
pub fn get_gpus() -> Vec<Gpu> {
    #[cfg(target_os = "linux")]
    {
        let mut gpus = Vec::new();
        gpus.extend(nvidia::get_gpus());
        gpus.extend(amd::get_gpus());
        gpus.extend(intel::get_gpus());
        gpus
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod nvidia {
    use super::Gpu;
    use libloading::{Library, Symbol};
    use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

    // nvmlReturn_t: 0 == NVML_SUCCESS.
    type NvmlReturn = c_int;
    const NVML_SUCCESS: NvmlReturn = 0;

    // Opaque device handle (nvmlDevice_t is a pointer).
    type NvmlDevice = *mut c_void;

    // nvmlMemory_t { total, free, used } — all u64.
    #[repr(C)]
    #[derive(Default)]
    struct NvmlMemory {
        total: u64,
        free: u64,
        used: u64,
    }

    // nvmlPciInfo_t. We only read busId (the formatted bus string); the struct
    // must match the C layout so the library writes within bounds.
    // busIdLegacy[16], domain, bus, device, pciDeviceId, pciSubSystemId,
    // busId[32].
    const NVML_DEVICE_PCI_BUS_ID_BUFFER_SIZE: usize = 32;
    #[repr(C)]
    struct NvmlPciInfo {
        bus_id_legacy: [c_char; 16],
        domain: c_uint,
        bus: c_uint,
        device: c_uint,
        pci_device_id: c_uint,
        pci_subsystem_id: c_uint,
        bus_id: [c_char; NVML_DEVICE_PCI_BUS_ID_BUFFER_SIZE],
    }

    type FnInit = unsafe extern "C" fn() -> NvmlReturn;
    type FnShutdown = unsafe extern "C" fn() -> NvmlReturn;
    type FnDeviceCount = unsafe extern "C" fn(*mut c_uint) -> NvmlReturn;
    type FnDeviceByIndex = unsafe extern "C" fn(c_uint, *mut NvmlDevice) -> NvmlReturn;
    type FnDeviceName = unsafe extern "C" fn(NvmlDevice, *mut c_char, c_uint) -> NvmlReturn;
    type FnDeviceMemory = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> NvmlReturn;
    type FnSystemDriver = unsafe extern "C" fn(*mut c_char, c_uint) -> NvmlReturn;
    type FnDevicePci = unsafe extern "C" fn(NvmlDevice, *mut NvmlPciInfo) -> NvmlReturn;
    type FnDeviceNuma = unsafe extern "C" fn(NvmlDevice, *mut c_int) -> NvmlReturn;
    type FnDeviceCudaCap = unsafe extern "C" fn(NvmlDevice, *mut c_int, *mut c_int) -> NvmlReturn;
    type FnDevicePcieGen = unsafe extern "C" fn(NvmlDevice, *mut c_uint) -> NvmlReturn;
    type FnDevicePcieWidth = unsafe extern "C" fn(NvmlDevice, *mut c_uint) -> NvmlReturn;
    type FnDeviceCores = unsafe extern "C" fn(NvmlDevice, *mut c_uint) -> NvmlReturn;

    /// Query the NVIDIA GPUs via NVML. Returns empty if NVML can't be loaded or
    /// initialized (e.g. no NVIDIA driver).
    pub fn get_gpus() -> Vec<Gpu> {
        // SAFETY: loading a system shared library is inherently unsafe; we trust
        // the NVIDIA-provided library and only call documented NVML functions.
        unsafe {
            let lib = match Library::new("libnvidia-ml.so.1")
                .or_else(|_| Library::new("libnvidia-ml.so"))
            {
                Ok(l) => l,
                Err(_) => return Vec::new(),
            };

            let init: Symbol<FnInit> = match lib.get(b"nvmlInit_v2") {
                Ok(s) => s,
                Err(_) => match lib.get(b"nvmlInit") {
                    Ok(s) => s,
                    Err(_) => return Vec::new(),
                },
            };
            if init() != NVML_SUCCESS {
                return Vec::new();
            }

            let gpus = collect(&lib);

            if let Ok(shutdown) = lib.get::<FnShutdown>(b"nvmlShutdown") {
                let _ = shutdown();
            }
            gpus
        }
    }

    unsafe fn collect(lib: &Library) -> Vec<Gpu> {
        let count_fn: Symbol<FnDeviceCount> = match lib.get(b"nvmlDeviceGetCount_v2") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let by_index: Symbol<FnDeviceByIndex> = match lib.get(b"nvmlDeviceGetHandleByIndex_v2") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        // Optional getters — missing on older NVML versions.
        let name_fn = lib.get::<FnDeviceName>(b"nvmlDeviceGetName").ok();
        let mem_fn = lib.get::<FnDeviceMemory>(b"nvmlDeviceGetMemoryInfo").ok();
        let pci_fn = lib.get::<FnDevicePci>(b"nvmlDeviceGetPciInfo_v3").ok();
        let numa_fn = lib.get::<FnDeviceNuma>(b"nvmlDeviceGetNumaNodeId").ok();
        let cap_fn = lib
            .get::<FnDeviceCudaCap>(b"nvmlDeviceGetCudaComputeCapability")
            .ok();
        let gen_fn = lib
            .get::<FnDevicePcieGen>(b"nvmlDeviceGetCurrPcieLinkGeneration")
            .ok();
        let width_fn = lib
            .get::<FnDevicePcieWidth>(b"nvmlDeviceGetCurrPcieLinkWidth")
            .ok();
        let cores_fn = lib.get::<FnDeviceCores>(b"nvmlDeviceGetNumGpuCores").ok();

        // Driver version is per-system, queried once.
        let driver = lib
            .get::<FnSystemDriver>(b"nvmlSystemGetDriverVersion")
            .ok()
            .and_then(|f| {
                let mut buf = [0 as c_char; 80];
                (f(buf.as_mut_ptr(), buf.len() as c_uint) == NVML_SUCCESS)
                    .then(|| cstr(&buf))
                    .flatten()
            });

        let mut count: c_uint = 0;
        if count_fn(&mut count) != NVML_SUCCESS {
            return Vec::new();
        }

        let mut gpus = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut device: NvmlDevice = std::ptr::null_mut();
            if by_index(index, &mut device) != NVML_SUCCESS {
                continue;
            }

            let name = name_fn.as_ref().and_then(|f| {
                let mut buf = [0 as c_char; 96];
                (f(device, buf.as_mut_ptr(), buf.len() as c_uint) == NVML_SUCCESS)
                    .then(|| cstr(&buf))
                    .flatten()
            });

            let memory_bytes = mem_fn.as_ref().and_then(|f| {
                let mut m = NvmlMemory::default();
                (f(device, &mut m) == NVML_SUCCESS).then_some(m.total)
            });

            let pci_bus_id = pci_fn.as_ref().and_then(|f| {
                let mut info: NvmlPciInfo = std::mem::zeroed();
                (f(device, &mut info) == NVML_SUCCESS)
                    .then(|| cstr(&info.bus_id))
                    .flatten()
            });

            let numa_node = numa_fn.as_ref().and_then(|f| {
                let mut node: c_int = -1;
                // -1 indicates "no NUMA node".
                (f(device, &mut node) == NVML_SUCCESS && node >= 0).then_some(node as usize)
            });

            let architecture = cap_fn.as_ref().and_then(|f| {
                let (mut major, mut minor): (c_int, c_int) = (0, 0);
                (f(device, &mut major, &mut minor) == NVML_SUCCESS)
                    .then(|| format!("{major}.{minor}"))
            });

            let pcie_gen = gen_fn.as_ref().and_then(|f| {
                let mut g: c_uint = 0;
                (f(device, &mut g) == NVML_SUCCESS).then_some(g as usize)
            });

            let pcie_width = width_fn.as_ref().and_then(|f| {
                let mut w: c_uint = 0;
                (f(device, &mut w) == NVML_SUCCESS).then_some(w as usize)
            });

            let cores = cores_fn.as_ref().and_then(|f| {
                let mut c: c_uint = 0;
                (f(device, &mut c) == NVML_SUCCESS).then_some(c as usize)
            });

            gpus.push(Gpu {
                index: index as usize,
                vendor: "nvidia".into(),
                name,
                memory_bytes,
                driver: driver.clone(),
                pci_bus_id,
                numa_node,
                architecture,
                pcie_gen,
                pcie_width,
                cores,
            });
        }
        gpus
    }

    /// Convert a NUL-terminated C char buffer to a Rust String, or None if empty.
    unsafe fn cstr(buf: &[c_char]) -> Option<String> {
        let s = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
        (!s.is_empty()).then_some(s)
    }
}

#[cfg(target_os = "linux")]
mod amd {
    use super::Gpu;
    use libloading::{Library, Symbol};
    use std::ffi::{c_char, CStr};

    // rsmi_status_t: 0 == RSMI_STATUS_SUCCESS.
    type RsmiStatus = u32;
    const RSMI_STATUS_SUCCESS: RsmiStatus = 0;
    // RSMI_MEM_TYPE_VRAM from rsmi_memory_type_t.
    const RSMI_MEM_TYPE_VRAM: u32 = 0;

    type FnInit = unsafe extern "C" fn(u64) -> RsmiStatus;
    type FnShutDown = unsafe extern "C" fn() -> RsmiStatus;
    type FnNumDevices = unsafe extern "C" fn(*mut u32) -> RsmiStatus;
    type FnName = unsafe extern "C" fn(u32, *mut c_char, usize) -> RsmiStatus;
    type FnNameU32Len = unsafe extern "C" fn(u32, *mut c_char, u32) -> RsmiStatus;
    type FnMemTotal = unsafe extern "C" fn(u32, u32, *mut u64) -> RsmiStatus;
    // rsmi_version_str_get(rsmi_sw_component_t component, char*, uint32_t len)
    type FnDriverVersion = unsafe extern "C" fn(u32, *mut c_char, u32) -> RsmiStatus;
    type FnPciId = unsafe extern "C" fn(u32, *mut u64) -> RsmiStatus;
    type FnNumaNode = unsafe extern "C" fn(u32, *mut i32) -> RsmiStatus;
    // rsmi_dev_target_graphics_version_get(uint32_t, uint64_t* gfx_version)
    type FnTargetGfx = unsafe extern "C" fn(u32, *mut u64) -> RsmiStatus;

    // RSMI_SW_COMP_DRIVER from rsmi_sw_component_t.
    const RSMI_SW_COMP_DRIVER: u32 = 0;

    /// Query the AMD GPUs via ROCm SMI. Returns empty if the library can't be
    /// loaded or initialized (e.g. no ROCm / AMD driver).
    pub fn get_gpus() -> Vec<Gpu> {
        // SAFETY: loading a system shared library is inherently unsafe; we trust
        // the ROCm-provided library and only call documented RSMI functions.
        unsafe {
            let lib = match Library::new("librocm_smi64.so")
                .or_else(|_| Library::new("librocm_smi64.so.1"))
            {
                Ok(l) => l,
                Err(_) => return Vec::new(),
            };

            let init: Symbol<FnInit> = match lib.get(b"rsmi_init") {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            if init(0) != RSMI_STATUS_SUCCESS {
                return Vec::new();
            }

            let gpus = collect(&lib);

            if let Ok(shut_down) = lib.get::<FnShutDown>(b"rsmi_shut_down") {
                let _ = shut_down();
            }
            gpus
        }
    }

    unsafe fn collect(lib: &Library) -> Vec<Gpu> {
        let num_fn: Symbol<FnNumDevices> = match lib.get(b"rsmi_num_monitor_devices") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        // Prefer the marketing name (e.g. "AMD Radeon AI PRO R9700"); fall back
        // to rsmi_dev_name_get (which may be a bare device id).
        let market_fn = lib.get::<FnNameU32Len>(b"rsmi_dev_market_name_get").ok();
        let name_fn = lib.get::<FnName>(b"rsmi_dev_name_get").ok();
        let mem_fn = lib.get::<FnMemTotal>(b"rsmi_dev_memory_total_get").ok();
        let pci_fn = lib.get::<FnPciId>(b"rsmi_dev_pci_id_get").ok();
        let numa_fn = lib
            .get::<FnNumaNode>(b"rsmi_topo_get_numa_node_number")
            .ok();
        let gfx_fn = lib
            .get::<FnTargetGfx>(b"rsmi_dev_target_graphics_version_get")
            .ok();

        // Driver version is per-system: rsmi_version_str_get(DRIVER, buf, len).
        let driver = lib
            .get::<FnDriverVersion>(b"rsmi_version_str_get")
            .ok()
            .and_then(|f| {
                let mut buf = [0 as c_char; 128];
                (f(RSMI_SW_COMP_DRIVER, buf.as_mut_ptr(), buf.len() as u32) == RSMI_STATUS_SUCCESS)
                    .then(|| cstr(&buf))
                    .flatten()
            });

        let mut count: u32 = 0;
        if num_fn(&mut count) != RSMI_STATUS_SUCCESS {
            return Vec::new();
        }

        let mut gpus = Vec::with_capacity(count as usize);
        for index in 0..count {
            let name = market_fn
                .as_ref()
                .and_then(|f| {
                    let mut buf = [0 as c_char; 256];
                    (f(index, buf.as_mut_ptr(), buf.len() as u32) == RSMI_STATUS_SUCCESS)
                        .then(|| cstr(&buf))
                        .flatten()
                })
                .or_else(|| {
                    name_fn.as_ref().and_then(|f| {
                        let mut buf = [0 as c_char; 256];
                        (f(index, buf.as_mut_ptr(), buf.len()) == RSMI_STATUS_SUCCESS)
                            .then(|| cstr(&buf))
                            .flatten()
                    })
                });

            let memory_bytes = mem_fn.as_ref().and_then(|f| {
                let mut bytes: u64 = 0;
                (f(index, RSMI_MEM_TYPE_VRAM, &mut bytes) == RSMI_STATUS_SUCCESS).then_some(bytes)
            });

            // PCI id is a packed 64-bit BDF: domain[63:32], bus[31:8],
            // device[7:3], function[2:0]. Format as the conventional string.
            let pci_bus_id = pci_fn.as_ref().and_then(|f| {
                let mut id: u64 = 0;
                (f(index, &mut id) == RSMI_STATUS_SUCCESS).then(|| {
                    let domain = (id >> 32) & 0xffff_ffff;
                    let bus = (id >> 8) & 0xff;
                    let device = (id >> 3) & 0x1f;
                    let function = id & 0x7;
                    format!("{domain:04x}:{bus:02x}:{device:02x}.{function}")
                })
            });

            let numa_node = numa_fn.as_ref().and_then(|f| {
                let mut node: i32 = -1;
                (f(index, &mut node) == RSMI_STATUS_SUCCESS && node >= 0).then_some(node as usize)
            });

            // gfx_version is the gfx target packed as hex digits, e.g. 0x1201
            // for gfx1201 and 0x90402 for gfx942 (CDNA). Render as "gfx{hex}".
            let architecture = gfx_fn.as_ref().and_then(|f| {
                let mut version: u64 = 0;
                (f(index, &mut version) == RSMI_STATUS_SUCCESS && version != 0)
                    .then(|| format!("gfx{version:x}"))
            });

            gpus.push(Gpu {
                index: index as usize,
                vendor: "amd".into(),
                name,
                memory_bytes,
                driver: driver.clone(),
                pci_bus_id,
                numa_node,
                architecture,
                // ROCm SMI does not expose static PCIe link gen/width or a
                // portable compute-unit count, so these are left as None.
                pcie_gen: None,
                pcie_width: None,
                cores: None,
            });
        }
        gpus
    }

    unsafe fn cstr(buf: &[c_char]) -> Option<String> {
        let s = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
        (!s.is_empty()).then_some(s)
    }
}

/// Intel GPU inventory, read from sysfs and the DRM query ioctl.
///
/// Unlike NVIDIA and AMD there is no userspace library to load: the i915/xe
/// driver publishes device identity through `/sys/class/drm/card*/device/`, and
/// the one thing sysfs does not carry — device-local memory size — comes from
/// the same `DRM_IOCTL_I915_QUERY` the `gpu_intel_pmu` sampler uses.
///
/// Both integrated and discrete parts are enumerated. They are distinguished by
/// whether the DRM query reports a device-local memory region: an integrated GPU
/// has none (it shares host RAM), which is also why `memory_bytes` is `None`
/// there rather than reporting system memory as if it were VRAM.
#[cfg(target_os = "linux")]
mod intel {
    use super::Gpu;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::path::{Path, PathBuf};

    /// Intel's PCI vendor ID.
    const PCI_VENDOR_INTEL: &str = "0x8086";

    /// `DRM_IOCTL_I915_QUERY`, from `DRM_IOWR(DRM_COMMAND_BASE + DRM_I915_QUERY, ...)`.
    const DRM_IOCTL_I915_QUERY: libc::c_ulong = 0xc0106479;
    /// `DRM_I915_QUERY_MEMORY_REGIONS`
    const QUERY_MEMORY_REGIONS: u64 = 4;
    /// `I915_MEMORY_CLASS_DEVICE` — card-local memory (VRAM).
    const MEMORY_CLASS_DEVICE: u16 = 1;

    #[repr(C)]
    struct DrmI915Query {
        num_items: u32,
        flags: u32,
        items_ptr: u64,
    }

    #[repr(C)]
    struct DrmI915QueryItem {
        query_id: u64,
        length: i32,
        flags: u32,
        data_ptr: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MemoryClassInstance {
        memory_class: u16,
        memory_instance: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MemoryRegionInfo {
        region: MemoryClassInstance,
        rsvd0: u32,
        probed_size: u64,
        unallocated_size: u64,
        rsvd1: [u64; 8],
    }

    #[repr(C)]
    struct QueryMemoryRegionsHeader {
        num_regions: u32,
        rsvd: [u32; 3],
    }

    /// Enumerate Intel GPUs. Returns empty on a host with none.
    pub fn get_gpus() -> Vec<Gpu> {
        let mut cards: Vec<(String, PathBuf)> = Vec::new();

        let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
            return Vec::new();
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();

            // Match `card1`, not connector children like `card1-DP-1`, and not
            // the render nodes (which describe the same device).
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }

            let device = entry.path().join("device");

            if read_trimmed(&device.join("vendor")).as_deref() != Some(PCI_VENDOR_INTEL) {
                continue;
            }

            // Only GPUs bound to a driver we understand; an unbound or
            // vfio-assigned device has no telemetry to correlate with.
            let driver = std::fs::canonicalize(device.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));

            if !matches!(driver.as_deref(), Some("i915") | Some("xe")) {
                continue;
            }

            cards.push((name, entry.path()));
        }

        // Stable, reproducible order: by card name, so `card1` precedes `card2`
        // and the index is not whatever order readdir happened to yield.
        cards.sort();

        cards
            .into_iter()
            .enumerate()
            .map(|(index, (_, card))| describe(index, &card))
            .collect()
    }

    fn describe(index: usize, card: &Path) -> Gpu {
        let device = card.join("device");

        let pci_bus_id = std::fs::canonicalize(&device)
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));

        let driver = std::fs::canonicalize(device.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));

        // NUMA node is -1 on a machine with no NUMA affinity for the device,
        // which is not a node number.
        let numa_node = read_trimmed(&device.join("numa_node"))
            .and_then(|v| v.parse::<i32>().ok())
            .and_then(|n| (n >= 0).then_some(n as usize));

        let vram = pci_bus_id.as_deref().and_then(vram_bytes);

        Gpu {
            index,
            vendor: "intel".to_string(),
            name: pci_bus_id.as_deref().and_then(device_name),
            // Only device-local memory counts as video memory. An integrated
            // GPU has none and reports `None` rather than passing host RAM off
            // as VRAM.
            memory_bytes: vram,
            driver,
            pci_bus_id,
            numa_node,
            // The i915/xe drivers expose no architecture string in sysfs. The
            // integrated/discrete split is the distinction that actually
            // changes how the metrics read (VRAM series exist only on
            // discrete), so it is what gets reported.
            architecture: Some(
                if vram.is_some() {
                    "discrete"
                } else {
                    "integrated"
                }
                .to_string(),
            ),
            // An integrated GPU sits on the root complex and reports "Unknown"
            // speed and width 0 — not a 0-lane link, but "no PCIe link here".
            // Both are reported as absent rather than as zero.
            //
            // On a discrete card these are the *current* link state, which the
            // ASPM/power-management layer downshifts when the GPU is idle: an
            // A770 capable of gen4 x16 reads gen1 x1 at rest and negotiates up
            // under load. That is the honest instantaneous answer, and matches
            // what NVML and ROCm SMI report for the other vendors.
            pcie_gen: read_trimmed(&device.join("current_link_speed")).and_then(|s| pcie_gen(&s)),
            pcie_width: read_trimmed(&device.join("current_link_width"))
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|w| *w > 0),
            // No EU count in sysfs; the OA/Metrics Discovery stack has it, but
            // that is a heavier dependency than a hardware inventory warrants.
            cores: None,
        }
    }

    /// Marketing name for a PCI device, from the system `pci.ids` database.
    ///
    /// The driver publishes only the numeric device ID, so the human-readable
    /// name has to be looked up. Absent that database the field is simply
    /// `None` — a missing name is better than a hex ID presented as one.
    fn device_name(pci_bus_id: &str) -> Option<String> {
        let device_path = Path::new("/sys/bus/pci/devices").join(pci_bus_id);
        let device_id = read_trimmed(&device_path.join("device"))?;
        let device_id = device_id.strip_prefix("0x")?.to_lowercase();

        let db = ["/usr/share/misc/pci.ids", "/usr/share/hwdata/pci.ids"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())?;

        // pci.ids is vendor-major: a vendor line at column 0, its devices
        // indented by one tab. Walk to Intel's block, then scan its devices.
        let mut in_intel = false;

        for line in db.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }

            if !line.starts_with('\t') {
                // A new vendor block begins; we are done once past Intel's.
                if in_intel {
                    break;
                }
                in_intel = line.starts_with("8086 ");
                continue;
            }

            if !in_intel || line.starts_with("\t\t") {
                continue;
            }

            let entry = line.trim_start();
            if let Some(rest) = entry.strip_prefix(&device_id) {
                let name = rest.trim();
                if !name.is_empty() {
                    return Some(format!("Intel {name}"));
                }
            }
        }

        None
    }

    /// PCIe generation from a sysfs link-speed string such as "16.0 GT/s PCIe".
    fn pcie_gen(speed: &str) -> Option<usize> {
        let gts: f64 = speed.split_whitespace().next()?.parse().ok()?;

        // Per-generation transfer rates; compared with tolerance because the
        // strings carry one decimal place.
        Some(match gts {
            g if g < 3.0 => 1,  // 2.5 GT/s
            g if g < 6.0 => 2,  // 5 GT/s
            g if g < 9.0 => 3,  // 8 GT/s
            g if g < 20.0 => 4, // 16 GT/s
            g if g < 40.0 => 5, // 32 GT/s
            _ => 6,             // 64 GT/s
        })
    }

    /// Total device-local memory (VRAM) in bytes, via the DRM query ioctl.
    ///
    /// Returns `None` for an integrated GPU (no device-local region) or when the
    /// render node cannot be opened. Unlike the sampler's live reading this uses
    /// only `probed_size`, which needs no special privilege — the unprivileged
    /// trap there affects `unallocated_size`, which an inventory does not want.
    fn vram_bytes(pci_bus_id: &str) -> Option<u64> {
        let file = render_node(pci_bus_id)?;

        let regions = query_memory_regions(&file)?;

        regions
            .iter()
            .find(|r| r.region.memory_class == MEMORY_CLASS_DEVICE)
            .map(|r| r.probed_size)
    }

    /// Open the render node (`renderD*`) whose PCI address matches.
    ///
    /// Render nodes need no DRM master and are the interface intended for
    /// non-display clients.
    fn render_node(pci_bus_id: &str) -> Option<File> {
        for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("renderD") {
                continue;
            }

            let target = std::fs::canonicalize(entry.path().join("device")).ok()?;
            if target.file_name().and_then(|s| s.to_str()) == Some(pci_bus_id) {
                return File::open(Path::new("/dev/dri").join(&name)).ok();
            }
        }

        None
    }

    fn query_memory_regions(file: &File) -> Option<Vec<MemoryRegionInfo>> {
        // Step 1: length probe. `length = 0` asks the kernel how many bytes the
        // reply needs.
        let mut item = DrmI915QueryItem {
            query_id: QUERY_MEMORY_REGIONS,
            length: 0,
            flags: 0,
            data_ptr: 0,
        };

        if !query(file, &mut item) || item.length <= 0 {
            return None;
        }

        let length = item.length as usize;
        let header_size = std::mem::size_of::<QueryMemoryRegionsHeader>();
        let region_size = std::mem::size_of::<MemoryRegionInfo>();

        if length < header_size {
            return None;
        }

        // Step 2: fetch into a buffer the kernel sized for us. `u64` backing
        // because both structs are 8-byte aligned.
        let words = length.div_ceil(std::mem::size_of::<u64>());
        let mut buffer: Vec<u64> = vec![0; words];

        item.data_ptr = buffer.as_mut_ptr() as u64;
        if !query(file, &mut item) {
            return None;
        }

        // SAFETY: `buffer` is at least `length` bytes, 8-byte aligned, and the
        // kernel has just written a `drm_i915_query_memory_regions` into it.
        let header = unsafe { &*(buffer.as_ptr() as *const QueryMemoryRegionsHeader) };

        // Trust the declared length over `num_regions`: only read as many
        // regions as actually fit in the buffer the kernel filled.
        let available = (length - header_size) / region_size;
        let count = (header.num_regions as usize).min(available);

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

    fn query(file: &File, item: &mut DrmI915QueryItem) -> bool {
        let mut q = DrmI915Query {
            num_items: 1,
            flags: 0,
            items_ptr: item as *mut DrmI915QueryItem as u64,
        };

        // SAFETY: `q` points at a valid `drm_i915_query` whose `items_ptr`
        // refers to a single valid `drm_i915_query_item`, matching what
        // DRM_IOCTL_I915_QUERY expects.
        let ret = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                DRM_IOCTL_I915_QUERY,
                &mut q as *mut DrmI915Query,
            )
        };

        ret == 0
    }

    fn read_trimmed(path: &Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn struct_layouts_match_the_uapi() {
            // Fixed by the kernel uapi; a mismatch would silently misparse.
            assert_eq!(std::mem::size_of::<DrmI915Query>(), 16);
            assert_eq!(std::mem::size_of::<DrmI915QueryItem>(), 24);
            assert_eq!(std::mem::size_of::<MemoryClassInstance>(), 4);
            assert_eq!(std::mem::size_of::<QueryMemoryRegionsHeader>(), 16);
            assert_eq!(std::mem::size_of::<MemoryRegionInfo>(), 88);
        }

        #[test]
        fn maps_link_speed_to_pcie_generation() {
            assert_eq!(pcie_gen("2.5 GT/s PCIe"), Some(1));
            assert_eq!(pcie_gen("5.0 GT/s PCIe"), Some(2));
            assert_eq!(pcie_gen("8.0 GT/s PCIe"), Some(3));
            assert_eq!(pcie_gen("16.0 GT/s PCIe"), Some(4));
            assert_eq!(pcie_gen("32.0 GT/s PCIe"), Some(5));
            assert_eq!(pcie_gen("Unknown"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On any platform, GPU discovery must not panic and the result must
    /// serialize. On hosts without a GPU library it returns an empty vector.
    #[test]
    fn get_gpus_does_not_panic_and_serializes() {
        let gpus = get_gpus();
        let _ = serde_json::to_string(&gpus).expect("gpus serialize");
    }
}
