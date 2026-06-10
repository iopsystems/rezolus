//! Minimal safe wrapper around the ROCm SMI library (`librocm_smi64.so`).
//!
//! Rather than depend on a `-sys` crate that runs `bindgen` against the ROCm
//! headers at build time (which would require ROCm to be installed wherever
//! Rezolus is compiled, including CI), we `dlopen` the shared library at
//! runtime via `libloading` and declare only the handful of C functions we
//! need. This mirrors how `nvml-wrapper` loads `libnvidia-ml.so`, so the agent
//! builds everywhere and only requires ROCm to be present at runtime on hosts
//! that actually have AMD GPUs.
//!
//! Signatures and enum values were taken from `rocm_smi/rocm_smi.h` (ROCm 7.x).

use libloading::{Library, Symbol};

/// `rsmi_status_t`: 0 (`RSMI_STATUS_SUCCESS`) indicates success.
type RsmiStatus = u32;
const RSMI_STATUS_SUCCESS: RsmiStatus = 0;

/// `RSMI_MEM_TYPE_VRAM` from `rsmi_memory_type_t`.
const RSMI_MEM_TYPE_VRAM: u32 = 0;

/// `RSMI_TEMP_CURRENT` from `rsmi_temperature_metric_t`.
const RSMI_TEMP_CURRENT: u32 = 0;

/// `RSMI_MAX_NUM_FREQUENCIES` from `rocm_smi.h`.
const RSMI_MAX_NUM_FREQUENCIES: usize = 33;

/// Sensors from `rsmi_temperature_type_t`.
#[derive(Clone, Copy)]
pub enum TempSensor {
    Edge = 0,
    Junction = 1,
    Memory = 2,
}

/// Clock domains from `rsmi_clk_type_t`.
#[derive(Clone, Copy)]
pub enum ClockType {
    /// System/graphics clock (`RSMI_CLK_TYPE_SYS`).
    System = 0,
    /// Memory clock (`RSMI_CLK_TYPE_MEM`).
    Memory = 4,
}

/// `rsmi_frequencies_t`.
#[repr(C)]
struct RsmiFrequencies {
    num_supported: u32,
    current: u32,
    frequency: [u64; RSMI_MAX_NUM_FREQUENCIES],
}

impl Default for RsmiFrequencies {
    fn default() -> Self {
        Self {
            num_supported: 0,
            current: 0,
            frequency: [0; RSMI_MAX_NUM_FREQUENCIES],
        }
    }
}

// Function pointer types for the ROCm SMI symbols we load.
type FnInit = unsafe extern "C" fn(u64) -> RsmiStatus;
type FnShutDown = unsafe extern "C" fn() -> RsmiStatus;
type FnNumDevices = unsafe extern "C" fn(*mut u32) -> RsmiStatus;
type FnMemTotal = unsafe extern "C" fn(u32, u32, *mut u64) -> RsmiStatus;
type FnMemUsage = unsafe extern "C" fn(u32, u32, *mut u64) -> RsmiStatus;
type FnBusyPercent = unsafe extern "C" fn(u32, *mut u32) -> RsmiStatus;
type FnTempMetric = unsafe extern "C" fn(u32, u32, u32, *mut i64) -> RsmiStatus;
type FnSocketPower = unsafe extern "C" fn(u32, *mut u64) -> RsmiStatus;
type FnPowerAve = unsafe extern "C" fn(u32, u32, *mut u64) -> RsmiStatus;
type FnEnergyCount = unsafe extern "C" fn(u32, *mut u64, *mut f32, *mut u64) -> RsmiStatus;
type FnClkFreq = unsafe extern "C" fn(u32, u32, *mut RsmiFrequencies) -> RsmiStatus;
type FnPciThroughput = unsafe extern "C" fn(u32, *mut u64, *mut u64, *mut u64) -> RsmiStatus;

/// A loaded ROCm SMI library with `rsmi_init()` already called.
///
/// `Drop` calls `rsmi_shut_down()`. The `_lib` field owns the dlopen handle and
/// must outlive every symbol; it is declared last so it drops last.
pub struct RocmSmi {
    shut_down: Symbol<'static, FnShutDown>,
    num_devices: Symbol<'static, FnNumDevices>,
    mem_total: Symbol<'static, FnMemTotal>,
    mem_usage: Symbol<'static, FnMemUsage>,
    busy_percent: Symbol<'static, FnBusyPercent>,
    mem_busy_percent: Symbol<'static, FnBusyPercent>,
    temp_metric: Symbol<'static, FnTempMetric>,
    socket_power: Option<Symbol<'static, FnSocketPower>>,
    power_ave: Option<Symbol<'static, FnPowerAve>>,
    energy_count: Option<Symbol<'static, FnEnergyCount>>,
    clk_freq: Symbol<'static, FnClkFreq>,
    pci_throughput: Option<Symbol<'static, FnPciThroughput>>,
    // SAFETY: must be dropped last; owns the memory the symbols point into.
    _lib: Box<Library>,
}

/// Look up a required symbol, returning an error if it is missing.
///
/// SAFETY: the returned `Symbol` borrows from `*lib`; we transmute its lifetime
/// to `'static` and rely on `RocmSmi` keeping `lib` alive (and dropping it
/// last) for soundness.
unsafe fn required<T>(lib: &Library, name: &[u8]) -> Result<Symbol<'static, T>, libloading::Error> {
    let sym: Symbol<T> = lib.get(name)?;
    Ok(std::mem::transmute::<Symbol<T>, Symbol<'static, T>>(sym))
}

/// Look up an optional symbol (some getters are absent on older ROCm).
unsafe fn optional<T>(lib: &Library, name: &[u8]) -> Option<Symbol<'static, T>> {
    let sym: Symbol<T> = lib.get(name).ok()?;
    Some(std::mem::transmute::<Symbol<T>, Symbol<'static, T>>(sym))
}

impl RocmSmi {
    /// Load the library, resolve symbols, and call `rsmi_init(0)`.
    pub fn new() -> Result<Self, String> {
        // SAFETY: loading an arbitrary shared library is inherently unsafe;
        // we trust the system-provided ROCm SMI library.
        let lib = unsafe {
            Library::new("librocm_smi64.so")
                .or_else(|_| Library::new("librocm_smi64.so.1"))
                .map_err(|e| format!("could not load librocm_smi64.so: {e}"))?
        };
        let lib = Box::new(lib);

        // SAFETY: signatures match rocm_smi.h; lib is kept alive by self.
        unsafe {
            let init: Symbol<FnInit> = lib
                .get(b"rsmi_init")
                .map_err(|e| format!("missing rsmi_init: {e}"))?;
            let status = init(0);
            if status != RSMI_STATUS_SUCCESS {
                return Err(format!("rsmi_init failed: status {status}"));
            }

            let map_err = |e: libloading::Error| format!("missing required symbol: {e}");

            let this = RocmSmi {
                shut_down: required(&lib, b"rsmi_shut_down").map_err(map_err)?,
                num_devices: required(&lib, b"rsmi_num_monitor_devices").map_err(map_err)?,
                mem_total: required(&lib, b"rsmi_dev_memory_total_get").map_err(map_err)?,
                mem_usage: required(&lib, b"rsmi_dev_memory_usage_get").map_err(map_err)?,
                busy_percent: required(&lib, b"rsmi_dev_busy_percent_get").map_err(map_err)?,
                mem_busy_percent: required(&lib, b"rsmi_dev_memory_busy_percent_get")
                    .map_err(map_err)?,
                temp_metric: required(&lib, b"rsmi_dev_temp_metric_get").map_err(map_err)?,
                socket_power: optional(&lib, b"rsmi_dev_current_socket_power_get"),
                power_ave: optional(&lib, b"rsmi_dev_power_ave_get"),
                energy_count: optional(&lib, b"rsmi_dev_energy_count_get"),
                clk_freq: required(&lib, b"rsmi_dev_gpu_clk_freq_get").map_err(map_err)?,
                pci_throughput: optional(&lib, b"rsmi_dev_pci_throughput_get"),
                _lib: lib,
            };

            Ok(this)
        }
    }

    /// Number of monitored devices.
    pub fn num_devices(&self) -> Result<usize, ()> {
        let mut n: u32 = 0;
        // SAFETY: out pointer is valid for the call.
        let status = unsafe { (self.num_devices)(&mut n) };
        if status == RSMI_STATUS_SUCCESS {
            Ok(n as usize)
        } else {
            Err(())
        }
    }

    /// Total VRAM in bytes.
    pub fn memory_total(&self, dv: usize) -> Result<u64, ()> {
        let mut v: u64 = 0;
        let status = unsafe { (self.mem_total)(dv as u32, RSMI_MEM_TYPE_VRAM, &mut v) };
        ok_or(status, v)
    }

    /// Used VRAM in bytes.
    pub fn memory_used(&self, dv: usize) -> Result<u64, ()> {
        let mut v: u64 = 0;
        let status = unsafe { (self.mem_usage)(dv as u32, RSMI_MEM_TYPE_VRAM, &mut v) };
        ok_or(status, v)
    }

    /// GPU busy percent (0-100).
    pub fn busy_percent(&self, dv: usize) -> Result<u32, ()> {
        let mut v: u32 = 0;
        let status = unsafe { (self.busy_percent)(dv as u32, &mut v) };
        ok_or(status, v)
    }

    /// Memory controller busy percent (0-100).
    pub fn memory_busy_percent(&self, dv: usize) -> Result<u32, ()> {
        let mut v: u32 = 0;
        let status = unsafe { (self.mem_busy_percent)(dv as u32, &mut v) };
        ok_or(status, v)
    }

    /// Current temperature in degrees Celsius for the given sensor.
    pub fn temperature(&self, dv: usize, sensor: TempSensor) -> Result<i64, ()> {
        let mut millideg: i64 = 0;
        let status = unsafe {
            (self.temp_metric)(dv as u32, sensor as u32, RSMI_TEMP_CURRENT, &mut millideg)
        };
        // ROCm reports temperature in millidegrees Celsius.
        ok_or(status, millideg / 1000)
    }

    /// Current power draw in milliwatts (socket power preferred, falling back to
    /// average power).
    pub fn power_milliwatts(&self, dv: usize) -> Result<u64, ()> {
        // Both getters report microwatts; convert to milliwatts.
        if let Some(f) = self.socket_power.as_ref() {
            let mut microwatts: u64 = 0;
            let status = unsafe { f(dv as u32, &mut microwatts) };
            if status == RSMI_STATUS_SUCCESS && microwatts > 0 {
                return Ok(microwatts / 1000);
            }
        }
        if let Some(f) = self.power_ave.as_ref() {
            let mut microwatts: u64 = 0;
            let status = unsafe { f(dv as u32, 0, &mut microwatts) };
            if status == RSMI_STATUS_SUCCESS {
                return Ok(microwatts / 1000);
            }
        }
        Err(())
    }

    /// Cumulative energy consumption in milliJoules.
    pub fn energy_millijoules(&self, dv: usize) -> Result<u64, ()> {
        let f = self.energy_count.as_ref().ok_or(())?;
        let mut energy: u64 = 0;
        let mut resolution: f32 = 0.0;
        let mut timestamp: u64 = 0;
        let status = unsafe { f(dv as u32, &mut energy, &mut resolution, &mut timestamp) };
        if status != RSMI_STATUS_SUCCESS || resolution <= 0.0 {
            return Err(());
        }
        // energy * resolution yields microJoules; convert to milliJoules.
        let micro_joules = energy as f64 * resolution as f64;
        Ok((micro_joules / 1000.0) as u64)
    }

    /// Current clock frequency in Hz for the given domain.
    pub fn clock_hz(&self, dv: usize, clk: ClockType) -> Result<u64, ()> {
        let mut freq = RsmiFrequencies::default();
        let status = unsafe { (self.clk_freq)(dv as u32, clk as u32, &mut freq) };
        if status != RSMI_STATUS_SUCCESS {
            return Err(());
        }
        let idx = freq.current as usize;
        if idx >= RSMI_MAX_NUM_FREQUENCIES {
            return Err(());
        }
        Ok(freq.frequency[idx])
    }

    /// PCIe throughput in bytes/second as `(sent, received)`.
    pub fn pcie_throughput(&self, dv: usize) -> Result<(u64, u64), ()> {
        let f = self.pci_throughput.as_ref().ok_or(())?;
        let mut sent: u64 = 0;
        let mut received: u64 = 0;
        let mut max_pkt_sz: u64 = 0;
        let status = unsafe { f(dv as u32, &mut sent, &mut received, &mut max_pkt_sz) };
        if status == RSMI_STATUS_SUCCESS {
            Ok((sent, received))
        } else {
            Err(())
        }
    }
}

impl Drop for RocmSmi {
    fn drop(&mut self) {
        // SAFETY: shut_down is valid until _lib drops, which happens after this.
        unsafe {
            let _ = (self.shut_down)();
        }
    }
}

#[inline]
fn ok_or<T>(status: RsmiStatus, value: T) -> Result<T, ()> {
    if status == RSMI_STATUS_SUCCESS {
        Ok(value)
    } else {
        Err(())
    }
}
