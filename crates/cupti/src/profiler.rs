use std::ffi::CStr;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use c_enum::c_enum;
use cupti_sys::*;

use crate::pmsampling::CounterDataImage;
use crate::util::CStringSlice;
use crate::{Error, Result};

c_enum! {
    /// Metric type classification.
    ///
    /// Categorizes metrics by their computational type.
    #[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
    pub enum MetricType : CUpti_MetricType {
        /// Counter metric type.
        Counter = CUPTI_METRIC_TYPE_COUNTER,

        /// Ratio metric type.
        Ratio = CUPTI_METRIC_TYPE_RATIO,

        /// Throughput metric type.
        Throughput = CUPTI_METRIC_TYPE_THROUGHPUT,
    }
}

c_enum! {
    /// Profiler type.
    ///
    /// Specifies the kind of profiler to use.
    #[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
    pub enum ProfilerType : CUpti_ProfilerType {
        /// Range-based profiler.
        RangeProfiler = CUPTI_PROFILER_TYPE_RANGE_PROFILER,

        /// PM sampling profiler.
        PmSampling = CUPTI_PROFILER_TYPE_PM_SAMPLING,

        /// Invalid profiler type.
        Invalid = CUPTI_PROFILER_TYPE_PROFILER_INVALID,
    }
}

pub struct HostProfiler {
    raw: NonNull<CUpti_Profiler_Host_Object>,
}

impl HostProfiler {
    pub fn new(
        ty: ProfilerType,
        chip_name: &CStr,
        counter_availability_image: &CounterAvailabilityImage,
    ) -> Result<Self> {
        let mut params = CUpti_Profiler_Host_Initialize_Params::default();
        params.structSize = CUpti_Profiler_Host_Initialize_Params_STRUCT_SIZE;
        params.profilerType = ty.into();
        params.pChipName = chip_name.as_ptr();
        params.pCounterAvailabilityImage = counter_availability_image.0.as_ptr();

        Error::result(unsafe { cuptiProfilerHostInitialize(&mut params) })?;

        let raw = match NonNull::new(params.pHostObject) {
            Some(raw) => raw,
            None => panic!("cuptiProfilerHostInitialize succeeded but returned null"),
        };

        Ok(Self { raw })
    }

    pub fn as_raw(&self) -> *const CUpti_Profiler_Host_Object {
        self.raw.as_ptr()
    }

    pub fn as_raw_mut(&mut self) -> *mut CUpti_Profiler_Host_Object {
        self.raw.as_ptr()
    }

    pub fn into_raw(self) -> *mut CUpti_Profiler_Host_Object {
        let mut this = ManuallyDrop::new(self);
        this.as_raw_mut()
    }

    /// Get the config image for the metrics added to the profiler host object.
    ///
    /// # Errors
    /// - [`Error::Unknown`] for any internal error.
    pub fn get_config_image(&self) -> Result<ConfigImage> {
        let mut params = CUpti_Profiler_Host_GetConfigImageSize_Params::default();
        params.structSize = CUpti_Profiler_Host_GetConfigImageSize_Params_STRUCT_SIZE;
        params.pHostObject = self.raw.as_ptr();

        Error::result(unsafe { cuptiProfilerHostGetConfigImageSize(&mut params) })?;

        let mut data = Vec::with_capacity(params.configImageSize);

        let mut params = CUpti_Profiler_Host_GetConfigImage_Params::default();
        params.structSize = CUpti_Profiler_Host_GetConfigImage_Params_STRUCT_SIZE;
        params.pHostObject = self.raw.as_ptr();
        params.configImageSize = data.capacity();
        params.pConfigImage = data.spare_capacity_mut().as_ptr() as *mut u8;

        Error::result(unsafe { cuptiProfilerHostGetConfigImage(&mut params) })?;
        unsafe { data.set_len(params.configImageSize) };

        Ok(ConfigImage(data))
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
        let mut params = CUpti_Profiler_Host_ConfigAddMetrics_Params::default();
        params.structSize = CUpti_Profiler_Host_ConfigAddMetrics_Params_STRUCT_SIZE;
        params.pHostObject = self.raw.as_ptr();
        params.ppMetricNames = metric_names.as_raw_slice().as_ptr() as *mut _;
        params.numMetrics = metric_names.as_raw_slice().len();

        Error::result(unsafe { cuptiProfilerHostConfigAddMetrics(&mut params) })
    }

    /// Evaluate the metric values for the range index stored in the counter
    /// data.
    ///
    /// # Params
    /// - `counter_data` - the counter data image where profiling data has been
    ///   decoded.
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
    pub fn evaluate_to_gpu_values(
        &self,
        counter_data: &CounterDataImage,
        range_index: usize,
        metric_names: &CStringSlice,
    ) -> Result<Vec<f64>> {
        let mut metric_values = Vec::with_capacity(metric_names.len());

        let mut params = CUpti_Profiler_Host_EvaluateToGpuValues_Params::default();
        params.structSize = CUpti_Profiler_Host_EvaluateToGpuValues_Params_STRUCT_SIZE;
        params.pHostObject = self.raw.as_ptr();
        params.pCounterDataImage = counter_data.as_bytes().as_ptr();
        params.counterDataImageSize = counter_data.as_bytes().len();
        params.rangeIndex = range_index;
        params.ppMetricNames = metric_names.as_raw_slice().as_ptr() as *mut _;
        params.numMetrics = metric_names.as_raw_slice().len();
        params.pMetricValues = metric_values.spare_capacity_mut().as_mut_ptr() as *mut _;

        Error::result(unsafe { cuptiProfilerHostEvaluateToGpuValues(&mut params) })?;
        unsafe { metric_values.set_len(params.numMetrics) };

        Ok(metric_values)
    }
}

impl Drop for HostProfiler {
    fn drop(&mut self) {
        let mut params = CUpti_Profiler_Host_Deinitialize_Params::default();
        params.structSize = CUpti_Profiler_Host_Deinitialize_Params_STRUCT_SIZE;
        params.pHostObject = self.raw.as_ptr();

        let _ = unsafe { cuptiProfilerHostDeinitialize(&mut params) };
    }
}

/// A config image containing info about the enabled metrics for the profiler.
#[derive(Clone)]
pub struct ConfigImage(Vec<u8>);

impl ConfigImage {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Get the number of passes required for profiling the scheduled metrics in
    /// this config image.
    ///
    /// # Errors
    /// - [`Error::InvalidParameter`] if this config image is invalid.
    /// - [`Error::Unknown`] for any internal error.
    pub fn get_num_of_passes(&self) -> Result<usize> {
        let mut params = CUpti_Profiler_Host_GetNumOfPasses_Params::default();
        params.structSize = CUpti_Profiler_Host_GetNumOfPasses_Params_STRUCT_SIZE;
        params.pConfigImage = self.0.as_ptr() as *mut u8;
        params.configImageSize = self.0.len();

        Error::result(unsafe { cuptiProfilerHostGetNumOfPasses(&mut params) })?;

        Ok(params.numOfPasses)
    }
}

/// Counter availability image.
///
/// This is used by CUPTI to filter out unavailable metrics on the host. For
/// users of the API it is effectively just an opaque blob of bytes.
#[derive(Clone)]
pub struct CounterAvailabilityImage(pub(crate) Vec<u8>);

impl CounterAvailabilityImage {
    /// Create from pre-existing bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
