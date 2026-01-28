use crate::agent::*;

use super::stats::*;

use cupti::pmsampling::{
    CounterDataImage, HardwareBufferAppendMode, Sampler as PmSampler, SamplerBuilder, TriggerMode,
};
use cupti::CStringList;
use std::ffi::CString;
use std::thread::JoinHandle;

// PM sampling configuration
const HARDWARE_BUFFER_SIZE: usize = 1024 * 1024; // 1MB hardware buffer
const SAMPLING_INTERVAL_CLOCKS: u64 = 10_000_000; // ~10ms at ~1GHz sysclk
const MAX_SAMPLES: u32 = 128;

pub struct CuptiSampler {
    thread: JoinHandle<()>,
    sync: SyncPrimitive,
}

impl CuptiSampler {
    pub fn new(device_count: usize) -> Option<Self> {
        match spawn_cupti_thread(device_count) {
            Ok((thread, sync)) => Some(Self { thread, sync }),
            Err(e) => {
                debug!("gpu_nvidia: CUPTI PM sampling unavailable: {e}");
                None
            }
        }
    }

    pub async fn sample(&self) -> anyhow::Result<()> {
        if self.thread.is_finished() {
            anyhow::bail!("CUPTI thread exited early");
        }

        self.sync.trigger();
        self.sync.wait_notify().await;

        Ok(())
    }
}

fn spawn_cupti_thread(device_count: usize) -> anyhow::Result<(JoinHandle<()>, SyncPrimitive)> {
    let sync = SyncPrimitive::new();
    let thread_sync = sync.clone();

    let thread = std::thread::spawn(move || {
        // Initialize CUPTI profiler interface
        let guard = match cupti::initialize() {
            Ok(g) => g,
            Err(e) => {
                // CUPTI_ERROR_UNKNOWN often indicates profiling is restricted.
                // Check: cat /proc/driver/nvidia/params | grep RmProfilingAdminOnly
                // If it shows "1", profiling requires admin mode to be disabled or
                // additional privileges (CAP_SYS_ADMIN).
                debug!("gpu_nvidia: failed to initialize CUPTI profiler: {e}");
                return;
            }
        };

        // Initialize samplers for devices we know exist (from NVML)
        let mut device_samplers = Vec::new();
        for device_index in 0..device_count {
            match init_device(device_index) {
                Ok(sampler) => {
                    debug!("gpu_nvidia: device {device_index} PM sampler initialized");
                    device_samplers.push(sampler);
                }
                Err(e) => {
                    debug!("gpu_nvidia: CUPTI not available for device {device_index}: {e}");
                }
            }
        }

        if device_samplers.is_empty() {
            debug!("gpu_nvidia: no devices with PM sampling support");
            return;
        }

        debug!(
            "gpu_nvidia: CUPTI PM sampling active for {} device(s)",
            device_samplers.len()
        );

        // Keep guard alive for the lifetime of the thread
        let _guard = guard;

        // Main sampling loop
        loop {
            thread_sync.wait_trigger();

            for (device_index, device) in device_samplers.iter_mut().enumerate() {
                if let Err(e) = sample_device(device_index, device) {
                    debug!("gpu_nvidia: device {device_index} PM sampling error: {e}");
                }
            }

            thread_sync.notify();
        }
    });

    // Give the thread time to initialize
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Check if the thread has already exited (indicating initialization failure)
    if thread.is_finished() {
        anyhow::bail!("CUPTI initialization thread exited early");
    }

    Ok((thread, sync))
}

struct DeviceSampler {
    sampler: PmSampler,
    counter_data: CounterDataImage,
    metric_names: CStringList,
}

fn init_device(device_index: usize) -> anyhow::Result<DeviceSampler> {
    let chip_name = cupti::get_device_chip_name(device_index)?;
    let chip_name_cstr = CString::new(chip_name)?;

    let counter_availability = PmSampler::get_counter_availability(device_index)?;

    let mut builder = SamplerBuilder::new(&chip_name_cstr, &counter_availability)?;

    // Use simpler metrics that fit in a single pass
    // sm__cycles_active / sm__cycles_elapsed gives SM utilization
    let metric_names: CStringList = [
        "gr__cycles_active.avg",
        "sm__cycles_active.avg",
        "sm__cycles_elapsed.avg",
    ]
    .iter()
    .filter_map(|&name| CString::new(name).ok())
    .collect();

    builder.add_metrics(&metric_names)?;

    let mut sampler = builder.build(device_index)?;

    // Check number of passes required - PM sampling only supports single pass
    if let Ok(num_passes) = sampler.get_num_passes() {
        if num_passes > 1 {
            anyhow::bail!(
                "PM sampling requires single-pass config, but {} passes needed",
                num_passes
            );
        }
    }

    // Use GpuSysclkInterval for Turing+ compatibility
    // Use KeepLatest mode to avoid OUT_OF_MEMORY errors on buffer overflow
    // (KeepLatest is supported on Ampere GA10x+)
    sampler.set_config(
        HARDWARE_BUFFER_SIZE,
        SAMPLING_INTERVAL_CLOCKS,
        TriggerMode::GpuSysclkInterval,
        HardwareBufferAppendMode::KeepLatest,
    )?;

    let metric_cstrs: Vec<_> = metric_names.iter().collect();
    let counter_data = CounterDataImage::new(&sampler, &metric_cstrs, MAX_SAMPLES)?;

    sampler.start()?;

    Ok(DeviceSampler {
        sampler,
        counter_data,
        metric_names,
    })
}

fn sample_device(device_index: usize, device: &mut DeviceSampler) -> anyhow::Result<()> {
    let _status = device.sampler.decode_data(&mut device.counter_data)?;

    let data_info = device.counter_data.get_data_info()?;

    if data_info.num_completed_samples == 0 {
        return Ok(());
    }

    let sample_index = data_info.num_completed_samples - 1;

    let values =
        device
            .counter_data
            .evaluate(&device.sampler, sample_index, &device.metric_names)?;

    // Current metrics:
    // [0] gr__cycles_active.avg - GR (graphics) cycles active
    // [1] sm__cycles_active.avg - SM cycles active
    // [2] sm__cycles_elapsed.avg - SM cycles elapsed
    //
    // SM utilization = sm__cycles_active / sm__cycles_elapsed * 100

    if let (Some(&cycles_active), Some(&cycles_elapsed)) = (values.get(1), values.get(2)) {
        if cycles_elapsed > 0.0 {
            let utilization = (cycles_active / cycles_elapsed * 100.0) as i64;
            let _ = GPU_SM_UTILIZATION.set(device_index, utilization.clamp(0, 100));
        }
    }

    Ok(())
}
