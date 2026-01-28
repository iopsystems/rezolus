//! Performance monitor sampling API.
//!
//! This module is a wrapper around the functions in `cupti_pmsampling.h`. It
//! allows youto collect a set of metrics by sampling the GPU's performance
//! monitors periodically at fixed intervals. Each sample is composed of metric
//! values and the GPU timestamp when it was collected in nanoseconds.
//!
//! These APIs here are supported on Turing and later GPU architectures.

use std::ffi::CStr;
use std::ptr::NonNull;

use c_enum::c_enum;
use cupti_sys::*;

use crate::profiler::*;
use crate::util::CStringSlice;
use crate::{Error, Result};

c_enum! {
    /// PM sampling trigger mode.
    ///
    /// Specifies the trigger mode for PM sampling which determines how the sampling interval
    /// is interpreted.
    #[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
    pub enum TriggerMode : CUpti_PmSampling_TriggerMode {
        /// The trigger is based off of the SYSCLK frequency.
        ///
        /// Note: SYS frequency by default is variable. The sample interval (set in
        /// [`CUpti_PmSampling_SetConfig_Params`]) is in terms of clocks.
        GpuSysclkInterval = CUPTI_PM_SAMPLING_TRIGGER_MODE_GPU_SYSCLK_INTERVAL,

        /// The trigger is based off of a fixed frequency source.
        ///
        /// The sample interval (set in [`CUpti_PmSampling_SetConfig_Params`]) is in terms of
        /// nanoseconds.
        ///
        /// # Notes
        ///
        /// This trigger mode is not supported on Turing GPU architecture and GA100 GPU.
        /// It is supported on Ampere GA10x and later GPU architectures.
        GpuTimeInterval = CUPTI_PM_SAMPLING_TRIGGER_MODE_GPU_TIME_INTERVAL,
    }
}

c_enum! {
    /// PM sampling decode stop reason.
    ///
    /// Indicates why the PM sampling decode operation stopped.
    #[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
    pub enum DecodeStopReason : CUpti_PmSampling_DecodeStopReason {
        /// Decode stopped for an unspecified reason.
        Other = CUPTI_PM_SAMPLING_DECODE_STOP_REASON_OTHER,

        /// Counter data image is full.
        CounterDataFull = CUPTI_PM_SAMPLING_DECODE_STOP_REASON_COUNTER_DATA_FULL,

        /// All the records in the hardware buffer have been decoded.
        EndOfRecords = CUPTI_PM_SAMPLING_DECODE_STOP_REASON_END_OF_RECORDS,
    }
}

c_enum! {
    /// Hardware buffer append mode for PM sampling.
    ///
    /// Specifies the behavior when the hardware buffer fills up during PM sampling.
    #[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
    pub enum HardwareBufferAppendMode : CUpti_PmSampling_HardwareBuffer_AppendMode {
        /// Keep the oldest records in the hardware buffer.
        ///
        /// CUPTI will report error for overflow in case hardware buffer is getting filled up.
        KeepOldest = CUPTI_PM_SAMPLING_HARDWARE_BUFFER_APPEND_MODE_KEEP_OLDEST,

        /// Keep the latest records in the hardware buffer.
        ///
        /// # Notes
        ///
        /// This mode is not supported on Turing GPU architecture.
        /// It is supported on Ampere and later GPU architectures.
        KeepLatest = CUPTI_PM_SAMPLING_HARDWARE_BUFFER_APPEND_MODE_KEEP_LATEST,
    }
}

pub struct SamplerBuilder {
    host: HostProfiler,
}

impl SamplerBuilder {
    /// Create and initialize a new builder for PM sampling.
    ///
    /// # Parameters
    ///
    /// - `chip_name`: The chip name (accepted for chips supported at the
    ///   time-of-release)
    /// - `counter_availability_image`: Buffer with counter availability image
    ///   (required for future chip support)
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::Unknown`] for any internal error
    pub fn new(
        chip_name: &CStr,
        counter_availability_image: &CounterAvailabilityImage,
    ) -> Result<Self> {
        let host = HostProfiler::new(
            ProfilerType::PmSampling,
            chip_name,
            counter_availability_image,
        )?;
        Ok(Self { host })
    }

    /// Construct a builder from a pre-configured host profiler.
    pub fn from_host_profiler(host: HostProfiler) -> Self {
        Self { host }
    }

    /// Add metrics to the profiler host object for generating the config image.
    ///
    /// The config image will have the required information to schedule the
    /// metrics for collecting the profiling data.
    ///
    /// # Parameters
    ///
    /// - `metric_names`: Metric names for which config image will be generated
    ///
    /// # Notes
    ///
    /// PM sampling only supports single pass config image.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InvalidMetricName`] if the metric name is not valid or not
    ///   supported for the chip
    /// - [`Error::Unknown`] for any internal error
    pub fn add_metrics(&mut self, metric_names: &CStringSlice) -> Result<()> {
        self.host.add_metrics(metric_names)
    }

    /// Create a PM sampling object and enable PM sampling on the CUDA device.
    ///
    /// # Parameters
    ///
    /// - `device_index`: Device index
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if `device_index` is not valid
    /// - [`Error::OutOfMemory`] if memory allocation fails while creating the
    ///   PM sampling object
    /// - [`Error::InvalidOperation`] if PM sampling is already enabled on the
    ///   device
    /// - [`Error::InsufficientPrivileges`] if the user does not have sufficient
    ///   privileges to perform the operation
    /// - [`Error::Unknown`] for any internal error
    pub fn build(self, device_index: usize) -> Result<Sampler> {
        let image = self.host.get_config_image()?;

        let mut params = CUpti_PmSampling_Enable_Params::default();
        params.structSize = CUpti_PmSampling_Enable_Params_STRUCT_SIZE;
        params.deviceIndex = device_index;

        Error::result(unsafe { cuptiPmSamplingEnable(&mut params) })?;

        let raw = NonNull::new(params.pPmSamplingObject).unwrap_or_else(|| {
            panic!("cuptiPmSamplingEnable succeeded but returne a null pointer")
        });

        Ok(Sampler {
            raw,
            host: self.host,
            config_image: image,
        })
    }
}

/// Collect a set of metrics from GPU performance monitors.
pub struct Sampler {
    raw: NonNull<CUpti_PmSampling_Object>,
    host: HostProfiler,
    config_image: ConfigImage,
}

impl Sampler {
    /// Create and initialize a new builder for PM sampling.
    ///
    /// # Parameters
    ///
    /// - `chip_name`: The chip name (accepted for chips supported at the
    ///   time-of-release)
    /// - `counter_availability_image`: Buffer with counter availability image
    ///   (required for future chip support)
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::Unknown`] for any internal error
    pub fn builder(
        chip_name: &CStr,
        counter_availability_image: &CounterAvailabilityImage,
    ) -> Result<SamplerBuilder> {
        SamplerBuilder::new(chip_name, counter_availability_image)
    }

    /// Get the number of passes required for the configured metrics.
    ///
    /// PM sampling only supports single-pass configurations. If this returns
    /// more than 1, you need to reduce the number of metrics.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if the config image is invalid
    /// - [`Error::Unknown`] for any internal error
    pub fn get_num_passes(&self) -> Result<usize> {
        self.config_image.get_num_of_passes()
    }

    /// Set the configuration for PM sampling like sampling interval, hardware
    /// buffer size, trigger mode and the config image which has scheduling info
    /// for metric collection.
    ///
    /// # Parameters
    ///
    /// - `hardware_buffer_size`: The hardware buffer size in which raw PM
    ///   sampling data will be stored. These samples will be decoded to counter
    ///   data image with [`decode_data`] call
    /// - `sampling_interval`: For the trigger mode
    ///   [`TriggerMode::GpuSysclkInterval`], sampling interval is the number of
    ///   sys clock cycles. For the trigger mode
    ///   [`TriggerMode::GpuTimeInterval`], sampling interval is in nanoseconds
    /// - `trigger_mode`: Trigger mode. Note: [`TriggerMode::GpuTimeInterval`]
    ///   is not supported in Turing and GA100. Supported from GA10x onwards
    /// - `hw_buffer_append_mode`: Append mode for the records in hardware
    ///   buffer. For [`HardwareBufferAppendMode::KeepOldest`] mode, all the
    ///   records will be kept in the buffer and in case hardware buffer is
    ///   getting filled up, overflow will be set to 1 in the decode status. For
    ///   [`HardwareBufferAppendMode::KeepLatest`] mode, the new records will
    ///   overwrite the oldest records in the buffer in case of filled buffer
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::NotSupported`] for config image which require multiple passes
    ///   for data collection
    ///
    /// [`decode_data`]: Self::decode_data
    pub fn set_config(
        &mut self,
        hardware_buffer_size: usize,
        sampling_interval: u64,
        trigger_mode: TriggerMode,
        hw_buffer_append_mode: HardwareBufferAppendMode,
    ) -> Result<()> {
        let mut params = CUpti_PmSampling_SetConfig_Params::default();
        params.structSize = CUpti_PmSampling_SetConfig_Params_STRUCT_SIZE;
        params.configSize = self.config_image.as_bytes().len();
        params.pPmSamplingObject = self.raw.as_ptr();
        params.pConfig = self.config_image.as_bytes().as_ptr();
        params.hardwareBufferSize = hardware_buffer_size;
        params.samplingInterval = sampling_interval;
        params.triggerMode = trigger_mode.into();
        params.hwBufferAppendMode = hw_buffer_append_mode.into();

        Error::result(unsafe { cuptiPmSamplingSetConfig(&mut params) })
    }

    /// Start the PM sampling.
    ///
    /// The GPU will start collecting the metrics data periodically based on
    /// trigger type and sampling interval passed in [`set_config`]. The
    /// collected data will be stored in the hardware buffer.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InvalidOperation`] if PM sampling Start is called without
    ///   enabling PM sampling, and PM sampling is already started
    /// - [`Error::Unknown`] for any internal error
    ///
    /// [`set_config`]: Self::set_config
    pub fn start(&mut self) -> Result<()> {
        let mut params = CUpti_PmSampling_Start_Params::default();
        params.structSize = CUpti_PmSampling_Start_Params_STRUCT_SIZE;
        params.pPmSamplingObject = self.raw.as_ptr();

        Error::result(unsafe { cuptiPmSamplingStart(&mut params) })
    }

    /// Stop the PM sampling.
    ///
    /// The GPU will stop collecting the metrics data.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InvalidOperation`] if PM sampling Stop is called without
    ///   enabling PM sampling, and PM sampling is already stopped
    /// - [`Error::Unknown`] for any internal error
    pub fn stop(&mut self) -> Result<()> {
        let mut params = CUpti_PmSampling_Stop_Params::default();
        params.structSize = CUpti_PmSampling_Stop_Params_STRUCT_SIZE;
        params.pPmSamplingObject = self.raw.as_ptr();

        Error::result(unsafe { cuptiPmSamplingStop(&mut params) })
    }

    /// Decode the metrics data stored in the hardware buffer to the counter
    /// data image.
    ///
    /// # Parameters
    ///
    /// - `counter_data`: Counter data image
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InvalidOperation`] if PM sampling DecodeData is called
    ///   without enabling PM sampling
    /// - [`Error::OutOfMemory`] if there is record overflow in the hardware
    ///   buffer
    /// - [`Error::Unknown`] for any internal error
    pub fn decode_data(&mut self, counter_data: &mut CounterDataImage) -> Result<DecodeStatus> {
        let mut params = CUpti_PmSampling_DecodeData_Params::default();
        params.structSize = CUpti_PmSampling_DecodeData_Params_STRUCT_SIZE;
        params.pPmSamplingObject = self.raw.as_ptr();
        params.pCounterDataImage = counter_data.0.as_mut_ptr();
        params.counterDataImageSize = counter_data.0.len();

        Error::result(unsafe { cuptiPmSamplingDecodeData(&mut params) })?;

        Ok(DecodeStatus {
            stop_reason: params.decodeStopReason.into(),
            overflow: params.overflow,
        })
    }

    /// Query counter availability information in a buffer which can be used to
    /// filter unavailable raw metrics on host.
    ///
    /// # Parameters
    ///
    /// - `device_index`: Device index
    ///
    /// # Notes
    ///
    /// This API may fail, if any profiling or sampling session is active on the
    /// specified device.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InsufficientPrivileges`] if the user does not have sufficient
    ///   privileges to perform the operation
    /// - [`Error::Unknown`] for any internal error
    pub fn get_counter_availability(device_index: usize) -> Result<CounterAvailabilityImage> {
        let mut params = CUpti_PmSampling_GetCounterAvailability_Params::default();
        params.structSize = CUpti_PmSampling_GetCounterAvailability_Params_STRUCT_SIZE;
        params.deviceIndex = device_index;

        Error::result(unsafe { cuptiPmSamplingGetCounterAvailability(&mut params) })?;
        params.pPriv = std::ptr::null_mut();

        let mut image = Vec::with_capacity(params.counterAvailabilityImageSize);
        params.pCounterAvailabilityImage = image.spare_capacity_mut().as_ptr() as *mut u8;

        Error::result(unsafe { cuptiPmSamplingGetCounterAvailability(&mut params) })?;
        unsafe { image.set_len(params.counterAvailabilityImageSize) };

        Ok(CounterAvailabilityImage(image))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        let mut params = CUpti_PmSampling_Disable_Params::default();
        params.structSize = CUpti_PmSampling_Disable_Params_STRUCT_SIZE;
        params.pPmSamplingObject = self.raw.as_ptr();

        let _ = unsafe { cuptiPmSamplingDisable(&mut params) };
    }
}

/// Status information returned from decoding PM sampling data.
#[derive(Copy, Clone, Debug)]
pub struct DecodeStatus {
    /// Decode stop reason
    pub stop_reason: DecodeStopReason,
    /// Overflow status for hardware buffer.
    ///
    /// To avoid overflow, either increase the hardware buffer size in
    /// [`PmSampler::set_config`] or reduce the sampling interval.
    pub overflow: u8,
}

/// A buffer storing decoded counter data.
///
/// You will need to create one of these before
pub struct CounterDataImage(Vec<u8>);

impl CounterDataImage {
    /// Create a counter data image buffer for storing metric data recorded by a
    /// [`PmSampler`].
    ///
    /// # Parameters
    ///
    /// - `sampler`: PM sampling object
    /// - `metric_names`: Names of the metrics to be collected
    /// - `max_samples`: Maximum number of samples to be stored in the counter
    ///   data image
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::InvalidOperation`] if called without enabling PM sampling
    /// - [`Error::Unknown`] for any internal error
    pub fn new(sampler: &Sampler, metric_names: &[&CStr], max_samples: u32) -> Result<Self> {
        let mut metric_names = metric_names
            .iter()
            .copied()
            .map(|c| c.as_ptr())
            .collect::<Vec<_>>();

        let mut params = CUpti_PmSampling_GetCounterDataSize_Params::default();
        params.structSize = CUpti_PmSampling_GetCounterDataSize_Params_STRUCT_SIZE;
        params.pPmSamplingObject = sampler.raw.as_ptr();
        params.pMetricNames = metric_names.as_mut_ptr();
        params.numMetrics = metric_names.len();
        params.maxSamples = max_samples;

        Error::result(unsafe { cuptiPmSamplingGetCounterDataSize(&mut params) })?;

        let mut image = Vec::with_capacity(params.counterDataSize);

        let mut params = CUpti_PmSampling_CounterDataImage_Initialize_Params::default();
        params.structSize = CUpti_PmSampling_CounterDataImage_Initialize_Params_STRUCT_SIZE;
        params.pPmSamplingObject = sampler.raw.as_ptr();
        params.counterDataSize = image.capacity();
        params.pCounterData = image.spare_capacity_mut().as_ptr() as *mut u8;

        Error::result(unsafe { cuptiPmSamplingCounterDataImageInitialize(&mut params) })?;

        unsafe { image.set_len(params.counterDataSize) };

        Ok(Self(image))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Get the counter data info like number of samples, number of populated
    /// samples and number of completed samples in a counter data image.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::Unknown`] for any internal error
    pub fn get_data_info(&self) -> Result<CounterDataInfo> {
        let mut params = CUpti_PmSampling_GetCounterDataInfo_Params::default();
        params.structSize = CUpti_PmSampling_GetCounterDataInfo_Params_STRUCT_SIZE;
        params.pCounterDataImage = self.0.as_ptr();
        params.counterDataImageSize = self.0.len();

        Error::result(unsafe { cuptiPmSamplingGetCounterDataInfo(&mut params) })?;

        Ok(CounterDataInfo {
            num_total_samples: params.numTotalSamples,
            num_populated_samples: params.numPopulatedSamples,
            num_completed_samples: params.numCompletedSamples,
        })
    }

    /// Get the sample info (start and end time stamp) for the given sample
    /// index.
    ///
    /// Each sample is distinguished by the start and end time stamp.
    ///
    /// # Parameters
    ///
    /// - `sampler`: PM sampling object
    /// - `sample_index`: Index of the sample
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidParameter`] if any parameter is not valid
    /// - [`Error::Unknown`] for any internal error
    pub fn get_sample_info(&self, sampler: &Sampler, sample_index: usize) -> Result<SampleInfo> {
        let mut params = CUpti_PmSampling_CounterData_GetSampleInfo_Params::default();
        params.structSize = CUpti_PmSampling_CounterData_GetSampleInfo_Params_STRUCT_SIZE;
        params.pPmSamplingObject = sampler.raw.as_ptr();
        params.pCounterDataImage = self.0.as_ptr();
        params.counterDataImageSize = self.0.len();
        params.sampleIndex = sample_index;

        Error::result(unsafe { cuptiPmSamplingCounterDataGetSampleInfo(&mut params) })?;

        Ok(SampleInfo {
            start_timestamp: params.startTimestamp,
            end_timestamp: params.endTimestamp,
        })
    }

    /// Evaluate the metric values for the range index stored in the counter
    /// data.
    ///
    /// # Params
    /// - `sampler` - the PM sampler.
    /// - `range_index` - the range index for which the range name will be
    ///   queried.
    /// - `metric_names` - the metrics for which GPU values will be evaluated
    ///   for the range.
    ///
    /// # Errors
    /// - [`Error::InvalidParameter`] if any of the parameters is not valid.
    /// - [`Error::InvalidMetricName`] if the metric name is not valid or not
    ///   supported.
    /// - [`Error::Unknown`] for any internal error.
    pub fn evaluate(
        &self,
        sampler: &Sampler,
        range_index: usize,
        metric_names: &CStringSlice,
    ) -> Result<Vec<f64>> {
        sampler
            .host
            .evaluate_to_gpu_values(self, range_index, metric_names)
    }
}

/// Time information for a PM sampling sample.
#[derive(Copy, Clone, Debug)]
pub struct SampleInfo {
    /// Start time of the sample
    pub start_timestamp: u64,
    /// End time of the sample
    pub end_timestamp: u64,
}

/// Information about samples in a counter data image.
#[derive(Copy, Clone, Debug)]
pub struct CounterDataInfo {
    /// Number of samples in the counter data image
    pub num_total_samples: usize,
    /// Number of populated samples
    pub num_populated_samples: usize,
    /// Number of samples that have been completed
    pub num_completed_samples: usize,
}
