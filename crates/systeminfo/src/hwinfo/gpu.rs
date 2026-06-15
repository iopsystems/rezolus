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
//! This is *static* hardware inventory only. Live telemetry (utilization,
//! temperature, power, clocks) belongs in the agent's GPU samplers, not here.

#![allow(non_camel_case_types)]

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gpu {
    /// Device index as reported by the vendor library.
    pub index: usize,
    /// Vendor identifier: "nvidia" or "amd".
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
    pub architecture: Option<String>,
    /// Current PCIe link generation (1-7).
    pub pcie_gen: Option<usize>,
    /// Current PCIe link width (number of lanes).
    pub pcie_width: Option<usize>,
    /// Number of compute cores:
    /// - NVIDIA: streaming multiprocessor (SM) count.
    /// - AMD: compute unit (CU) count.
    pub cores: Option<usize>,
}

/// Discover all GPUs on the system, querying every available vendor backend.
pub fn get_gpus() -> Vec<Gpu> {
    #[cfg(target_os = "linux")]
    {
        let mut gpus = Vec::new();
        gpus.extend(nvidia::get_gpus());
        gpus.extend(amd::get_gpus());
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
        let cap_fn = lib.get::<FnDeviceCudaCap>(b"nvmlDeviceGetCudaComputeCapability").ok();
        let gen_fn = lib.get::<FnDevicePcieGen>(b"nvmlDeviceGetCurrPcieLinkGeneration").ok();
        let width_fn = lib.get::<FnDevicePcieWidth>(b"nvmlDeviceGetCurrPcieLinkWidth").ok();
        let cores_fn = lib.get::<FnDeviceCores>(b"nvmlDeviceGetNumGpuCores").ok();

        // Driver version is per-system, queried once.
        let driver = lib.get::<FnSystemDriver>(b"nvmlSystemGetDriverVersion").ok().and_then(
            |f| {
                let mut buf = [0 as c_char; 80];
                (f(buf.as_mut_ptr(), buf.len() as c_uint) == NVML_SUCCESS)
                    .then(|| cstr(&buf))
                    .flatten()
            },
        );

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
            let lib =
                match Library::new("librocm_smi64.so").or_else(|_| Library::new("librocm_smi64.so.1")) {
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
        let numa_fn = lib.get::<FnNumaNode>(b"rsmi_topo_get_numa_node_number").ok();
        let gfx_fn = lib.get::<FnTargetGfx>(b"rsmi_dev_target_graphics_version_get").ok();

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
