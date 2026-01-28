//! Hand-written minimal bindings for CUPTI PM sampling.
//!
//! These bindings are designed to work across CUDA 11, 12, and 13 by:
//! 1. Using runtime dynamic loading (dlopen) instead of linking
//! 2. Using CUPTI's structSize versioning for ABI compatibility
//! 3. Only exposing the minimal API surface needed for PM sampling

#![allow(nonstandard_style)]
#![allow(clippy::all)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int, c_uint};
use std::sync::OnceLock;

use libloading::Library;

// =============================================================================
// Type Aliases
// =============================================================================

/// CUDA result type (success = 0)
pub type CUresult = c_uint;
/// CUDA device handle
pub type CUdevice = c_int;
/// CUPTI result type (success = 0)
pub type CUptiResult = c_uint;

/// Profiler type enum
pub type CUpti_ProfilerType = c_uint;
/// Metric type enum
pub type CUpti_MetricType = c_uint;
/// PM sampling trigger mode enum
pub type CUpti_PmSampling_TriggerMode = c_uint;
/// PM sampling decode stop reason enum
pub type CUpti_PmSampling_DecodeStopReason = c_uint;
/// PM sampling hardware buffer append mode enum
pub type CUpti_PmSampling_HardwareBuffer_AppendMode = c_uint;

// =============================================================================
// Constants
// =============================================================================

// CUDA result codes
pub const CUDA_SUCCESS: CUresult = 0;

// CUPTI result codes
pub const CUPTI_SUCCESS: CUptiResult = 0;
pub const CUPTI_ERROR_INVALID_PARAMETER: CUptiResult = 1;
pub const CUPTI_ERROR_INVALID_DEVICE: CUptiResult = 2;
pub const CUPTI_ERROR_INVALID_CONTEXT: CUptiResult = 3;
pub const CUPTI_ERROR_INVALID_EVENT_DOMAIN_ID: CUptiResult = 4;
pub const CUPTI_ERROR_INVALID_EVENT_ID: CUptiResult = 5;
pub const CUPTI_ERROR_INVALID_EVENT_NAME: CUptiResult = 6;
pub const CUPTI_ERROR_INVALID_OPERATION: CUptiResult = 7;
pub const CUPTI_ERROR_OUT_OF_MEMORY: CUptiResult = 8;
pub const CUPTI_ERROR_HARDWARE: CUptiResult = 9;
pub const CUPTI_ERROR_PARAMETER_SIZE_NOT_SUFFICIENT: CUptiResult = 10;
pub const CUPTI_ERROR_NOT_INITIALIZED: CUptiResult = 15;
pub const CUPTI_ERROR_NOT_COMPATIBLE: CUptiResult = 16;
pub const CUPTI_ERROR_NOT_SUPPORTED: CUptiResult = 17;
pub const CUPTI_ERROR_INVALID_METRIC_NAME: CUptiResult = 21;
pub const CUPTI_ERROR_INSUFFICIENT_PRIVILEGES: CUptiResult = 25;
pub const CUPTI_ERROR_UNKNOWN: CUptiResult = 999;

// Profiler types
pub const CUPTI_PROFILER_TYPE_RANGE_PROFILER: CUpti_ProfilerType = 0;
pub const CUPTI_PROFILER_TYPE_PM_SAMPLING: CUpti_ProfilerType = 1;
pub const CUPTI_PROFILER_TYPE_PROFILER_INVALID: CUpti_ProfilerType = 2;

// Metric types
pub const CUPTI_METRIC_TYPE_COUNTER: CUpti_MetricType = 0;
pub const CUPTI_METRIC_TYPE_RATIO: CUpti_MetricType = 1;
pub const CUPTI_METRIC_TYPE_THROUGHPUT: CUpti_MetricType = 2;

// PM sampling trigger modes
pub const CUPTI_PM_SAMPLING_TRIGGER_MODE_GPU_SYSCLK_INTERVAL: CUpti_PmSampling_TriggerMode = 0;
pub const CUPTI_PM_SAMPLING_TRIGGER_MODE_GPU_TIME_INTERVAL: CUpti_PmSampling_TriggerMode = 1;

// PM sampling decode stop reasons
pub const CUPTI_PM_SAMPLING_DECODE_STOP_REASON_OTHER: CUpti_PmSampling_DecodeStopReason = 0;
pub const CUPTI_PM_SAMPLING_DECODE_STOP_REASON_COUNTER_DATA_FULL:
    CUpti_PmSampling_DecodeStopReason = 1;
pub const CUPTI_PM_SAMPLING_DECODE_STOP_REASON_END_OF_RECORDS: CUpti_PmSampling_DecodeStopReason =
    2;

// Hardware buffer append modes
pub const CUPTI_PM_SAMPLING_HARDWARE_BUFFER_APPEND_MODE_KEEP_OLDEST:
    CUpti_PmSampling_HardwareBuffer_AppendMode = 0;
pub const CUPTI_PM_SAMPLING_HARDWARE_BUFFER_APPEND_MODE_KEEP_LATEST:
    CUpti_PmSampling_HardwareBuffer_AppendMode = 1;

// =============================================================================
// Opaque Types
// =============================================================================

/// Opaque PM sampling object (handle returned by CUPTI)
#[repr(C)]
pub struct CUpti_PmSampling_Object {
    _private: [u8; 0],
}

/// Opaque profiler host object (handle returned by CUPTI)
#[repr(C)]
pub struct CUpti_Profiler_Host_Object {
    _private: [u8; 0],
}

// =============================================================================
// Parameter Structures
// =============================================================================

// All CUPTI param structs follow this pattern:
// - First field: structSize (usize) - set to size of struct up to last used field
// - Second field: pPriv (*mut c_void) - always set to NULL
// - Remaining fields: input/output parameters

/// Parameters for cuptiProfilerInitialize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Initialize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
}

/// Parameters for cuptiProfilerDeInitialize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_DeInitialize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
}

/// Parameters for cuptiDeviceGetChipName
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Device_GetChipName_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub deviceIndex: usize,
    pub pChipName: *const c_char,
}

/// Parameters for cuptiProfilerHostInitialize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_Initialize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub profilerType: CUpti_ProfilerType,
    pub pChipName: *const c_char,
    pub pCounterAvailabilityImage: *const u8,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
}

/// Parameters for cuptiProfilerHostDeinitialize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_Deinitialize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
}

/// Parameters for cuptiProfilerHostConfigAddMetrics
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_ConfigAddMetrics_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
    pub ppMetricNames: *mut *const c_char,
    pub numMetrics: usize,
}

/// Parameters for cuptiProfilerHostGetConfigImageSize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_GetConfigImageSize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
    pub configImageSize: usize,
}

/// Parameters for cuptiProfilerHostGetConfigImage
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_GetConfigImage_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
    pub configImageSize: usize,
    pub pConfigImage: *mut u8,
}

/// Parameters for cuptiProfilerHostGetNumOfPasses
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_GetNumOfPasses_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub configImageSize: usize,
    pub pConfigImage: *mut u8,
    pub numOfPasses: usize,
}

/// Parameters for cuptiProfilerHostEvaluateToGpuValues
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_Profiler_Host_EvaluateToGpuValues_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pHostObject: *mut CUpti_Profiler_Host_Object,
    pub pCounterDataImage: *const u8,
    pub counterDataImageSize: usize,
    pub rangeIndex: usize,
    pub ppMetricNames: *mut *const c_char,
    pub numMetrics: usize,
    pub pMetricValues: *mut f64,
}

/// Parameters for cuptiPmSamplingEnable
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_Enable_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub deviceIndex: usize,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
}

/// Parameters for cuptiPmSamplingDisable
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_Disable_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
}

/// Parameters for cuptiPmSamplingSetConfig
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_SetConfig_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
    pub configSize: usize,
    pub pConfig: *const u8,
    pub hardwareBufferSize: usize,
    pub samplingInterval: u64,
    pub triggerMode: CUpti_PmSampling_TriggerMode,
    pub hwBufferAppendMode: CUpti_PmSampling_HardwareBuffer_AppendMode,
}

/// Parameters for cuptiPmSamplingStart
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_Start_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
}

/// Parameters for cuptiPmSamplingStop
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_Stop_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
}

/// Parameters for cuptiPmSamplingDecodeData
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_DecodeData_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
    pub pCounterDataImage: *mut u8,
    pub counterDataImageSize: usize,
    pub decodeStopReason: CUpti_PmSampling_DecodeStopReason,
    pub overflow: u8,
}

/// Parameters for cuptiPmSamplingGetCounterAvailability
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_GetCounterAvailability_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub deviceIndex: usize,
    pub counterAvailabilityImageSize: usize,
    pub pCounterAvailabilityImage: *mut u8,
}

/// Parameters for cuptiPmSamplingGetCounterDataSize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_GetCounterDataSize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
    pub pMetricNames: *mut *const c_char,
    pub numMetrics: usize,
    pub maxSamples: u32,
    pub counterDataSize: usize,
}

/// Parameters for cuptiPmSamplingCounterDataImageInitialize
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_CounterDataImage_Initialize_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
    pub counterDataSize: usize,
    pub pCounterData: *mut u8,
}

/// Parameters for cuptiPmSamplingGetCounterDataInfo
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_GetCounterDataInfo_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pCounterDataImage: *const u8,
    pub counterDataImageSize: usize,
    pub numTotalSamples: usize,
    pub numPopulatedSamples: usize,
    pub numCompletedSamples: usize,
}

/// Parameters for cuptiPmSamplingCounterDataGetSampleInfo
#[repr(C)]
#[derive(Debug, Default)]
pub struct CUpti_PmSampling_CounterData_GetSampleInfo_Params {
    pub structSize: usize,
    pub pPriv: *mut c_void,
    pub pPmSamplingObject: *mut CUpti_PmSampling_Object,
    pub pCounterDataImage: *const u8,
    pub counterDataImageSize: usize,
    pub sampleIndex: usize,
    pub startTimestamp: u64,
    pub endTimestamp: u64,
}

// =============================================================================
// Struct Size Constants
// =============================================================================

// These are computed as offset(last_field) + sizeof(last_field)
// Using mem::offset_of! and mem::size_of! for correctness

macro_rules! struct_size {
    ($type:ty, $last_field:ident) => {{
        std::mem::offset_of!($type, $last_field)
            + std::mem::size_of::<<$type as StructLastField>::LastFieldType>()
    }};
}

// Helper trait to get the type of the last field
trait StructLastField {
    type LastFieldType;
}

impl StructLastField for CUpti_Profiler_Initialize_Params {
    type LastFieldType = *mut c_void;
}
impl StructLastField for CUpti_Profiler_DeInitialize_Params {
    type LastFieldType = *mut c_void;
}
impl StructLastField for CUpti_Device_GetChipName_Params {
    type LastFieldType = *const c_char;
}
impl StructLastField for CUpti_Profiler_Host_Initialize_Params {
    type LastFieldType = *mut CUpti_Profiler_Host_Object;
}
impl StructLastField for CUpti_Profiler_Host_Deinitialize_Params {
    type LastFieldType = *mut CUpti_Profiler_Host_Object;
}
impl StructLastField for CUpti_Profiler_Host_ConfigAddMetrics_Params {
    type LastFieldType = usize;
}
impl StructLastField for CUpti_Profiler_Host_GetConfigImageSize_Params {
    type LastFieldType = usize;
}
impl StructLastField for CUpti_Profiler_Host_GetConfigImage_Params {
    type LastFieldType = *mut u8;
}
impl StructLastField for CUpti_Profiler_Host_GetNumOfPasses_Params {
    type LastFieldType = usize;
}
impl StructLastField for CUpti_Profiler_Host_EvaluateToGpuValues_Params {
    type LastFieldType = *mut f64;
}
impl StructLastField for CUpti_PmSampling_Enable_Params {
    type LastFieldType = *mut CUpti_PmSampling_Object;
}
impl StructLastField for CUpti_PmSampling_Disable_Params {
    type LastFieldType = *mut CUpti_PmSampling_Object;
}
impl StructLastField for CUpti_PmSampling_SetConfig_Params {
    type LastFieldType = CUpti_PmSampling_HardwareBuffer_AppendMode;
}
impl StructLastField for CUpti_PmSampling_Start_Params {
    type LastFieldType = *mut CUpti_PmSampling_Object;
}
impl StructLastField for CUpti_PmSampling_Stop_Params {
    type LastFieldType = *mut CUpti_PmSampling_Object;
}
impl StructLastField for CUpti_PmSampling_DecodeData_Params {
    type LastFieldType = u8;
}
impl StructLastField for CUpti_PmSampling_GetCounterAvailability_Params {
    type LastFieldType = *mut u8;
}
impl StructLastField for CUpti_PmSampling_GetCounterDataSize_Params {
    type LastFieldType = usize;
}
impl StructLastField for CUpti_PmSampling_CounterDataImage_Initialize_Params {
    type LastFieldType = *mut u8;
}
impl StructLastField for CUpti_PmSampling_GetCounterDataInfo_Params {
    type LastFieldType = usize;
}
impl StructLastField for CUpti_PmSampling_CounterData_GetSampleInfo_Params {
    type LastFieldType = u64;
}

pub const CUpti_Profiler_Initialize_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Initialize_Params, pPriv);
pub const CUpti_Profiler_DeInitialize_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_DeInitialize_Params, pPriv);
pub const CUpti_Device_GetChipName_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Device_GetChipName_Params, pChipName);
pub const CUpti_Profiler_Host_Initialize_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Host_Initialize_Params, pHostObject);
pub const CUpti_Profiler_Host_Deinitialize_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Host_Deinitialize_Params, pHostObject);
pub const CUpti_Profiler_Host_ConfigAddMetrics_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Host_ConfigAddMetrics_Params, numMetrics);
pub const CUpti_Profiler_Host_GetConfigImageSize_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_Profiler_Host_GetConfigImageSize_Params,
    configImageSize
);
pub const CUpti_Profiler_Host_GetConfigImage_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Host_GetConfigImage_Params, pConfigImage);
pub const CUpti_Profiler_Host_GetNumOfPasses_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_Profiler_Host_GetNumOfPasses_Params, numOfPasses);
pub const CUpti_Profiler_Host_EvaluateToGpuValues_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_Profiler_Host_EvaluateToGpuValues_Params,
    pMetricValues
);
pub const CUpti_PmSampling_Enable_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_Enable_Params, pPmSamplingObject);
pub const CUpti_PmSampling_Disable_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_Disable_Params, pPmSamplingObject);
pub const CUpti_PmSampling_SetConfig_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_SetConfig_Params, hwBufferAppendMode);
pub const CUpti_PmSampling_Start_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_Start_Params, pPmSamplingObject);
pub const CUpti_PmSampling_Stop_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_Stop_Params, pPmSamplingObject);
pub const CUpti_PmSampling_DecodeData_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_DecodeData_Params, overflow);
pub const CUpti_PmSampling_GetCounterAvailability_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_PmSampling_GetCounterAvailability_Params,
    pCounterAvailabilityImage
);
pub const CUpti_PmSampling_GetCounterDataSize_Params_STRUCT_SIZE: usize =
    struct_size!(CUpti_PmSampling_GetCounterDataSize_Params, counterDataSize);
pub const CUpti_PmSampling_CounterDataImage_Initialize_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_PmSampling_CounterDataImage_Initialize_Params,
    pCounterData
);
pub const CUpti_PmSampling_GetCounterDataInfo_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_PmSampling_GetCounterDataInfo_Params,
    numCompletedSamples
);
pub const CUpti_PmSampling_CounterData_GetSampleInfo_Params_STRUCT_SIZE: usize = struct_size!(
    CUpti_PmSampling_CounterData_GetSampleInfo_Params,
    endTimestamp
);

// =============================================================================
// Function Types
// =============================================================================

// CUDA driver functions
type FnCuInit = unsafe extern "C" fn(flags: c_uint) -> CUresult;
type FnCuDeviceGet = unsafe extern "C" fn(device: *mut CUdevice, ordinal: c_int) -> CUresult;

// CUPTI profiler functions
type FnCuptiProfilerInitialize =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Initialize_Params) -> CUptiResult;
type FnCuptiProfilerDeInitialize =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_DeInitialize_Params) -> CUptiResult;
type FnCuptiDeviceGetChipName =
    unsafe extern "C" fn(params: *mut CUpti_Device_GetChipName_Params) -> CUptiResult;
type FnCuptiProfilerHostInitialize =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_Initialize_Params) -> CUptiResult;
type FnCuptiProfilerHostDeinitialize =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_Deinitialize_Params) -> CUptiResult;
type FnCuptiProfilerHostConfigAddMetrics =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_ConfigAddMetrics_Params) -> CUptiResult;
type FnCuptiProfilerHostGetConfigImageSize =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_GetConfigImageSize_Params) -> CUptiResult;
type FnCuptiProfilerHostGetConfigImage =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_GetConfigImage_Params) -> CUptiResult;
type FnCuptiProfilerHostGetNumOfPasses =
    unsafe extern "C" fn(params: *mut CUpti_Profiler_Host_GetNumOfPasses_Params) -> CUptiResult;
type FnCuptiProfilerHostEvaluateToGpuValues = unsafe extern "C" fn(
    params: *mut CUpti_Profiler_Host_EvaluateToGpuValues_Params,
) -> CUptiResult;

// CUPTI PM sampling functions
type FnCuptiPmSamplingEnable =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_Enable_Params) -> CUptiResult;
type FnCuptiPmSamplingDisable =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_Disable_Params) -> CUptiResult;
type FnCuptiPmSamplingSetConfig =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_SetConfig_Params) -> CUptiResult;
type FnCuptiPmSamplingStart =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_Start_Params) -> CUptiResult;
type FnCuptiPmSamplingStop =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_Stop_Params) -> CUptiResult;
type FnCuptiPmSamplingDecodeData =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_DecodeData_Params) -> CUptiResult;
type FnCuptiPmSamplingGetCounterAvailability = unsafe extern "C" fn(
    params: *mut CUpti_PmSampling_GetCounterAvailability_Params,
) -> CUptiResult;
type FnCuptiPmSamplingGetCounterDataSize =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_GetCounterDataSize_Params) -> CUptiResult;
type FnCuptiPmSamplingCounterDataImageInitialize = unsafe extern "C" fn(
    params: *mut CUpti_PmSampling_CounterDataImage_Initialize_Params,
) -> CUptiResult;
type FnCuptiPmSamplingGetCounterDataInfo =
    unsafe extern "C" fn(params: *mut CUpti_PmSampling_GetCounterDataInfo_Params) -> CUptiResult;
type FnCuptiPmSamplingCounterDataGetSampleInfo = unsafe extern "C" fn(
    params: *mut CUpti_PmSampling_CounterData_GetSampleInfo_Params,
) -> CUptiResult;

// =============================================================================
// Library Loading
// =============================================================================

/// Holds references to the loaded CUDA and CUPTI libraries
struct CuptiLibraries {
    _cuda: Library,
    _cupti: Library,

    // CUDA functions
    cu_init: FnCuInit,
    cu_device_get: FnCuDeviceGet,

    // CUPTI profiler functions
    cupti_profiler_initialize: FnCuptiProfilerInitialize,
    cupti_profiler_deinitialize: FnCuptiProfilerDeInitialize,
    cupti_device_get_chip_name: FnCuptiDeviceGetChipName,
    cupti_profiler_host_initialize: FnCuptiProfilerHostInitialize,
    cupti_profiler_host_deinitialize: FnCuptiProfilerHostDeinitialize,
    cupti_profiler_host_config_add_metrics: FnCuptiProfilerHostConfigAddMetrics,
    cupti_profiler_host_get_config_image_size: FnCuptiProfilerHostGetConfigImageSize,
    cupti_profiler_host_get_config_image: FnCuptiProfilerHostGetConfigImage,
    cupti_profiler_host_get_num_of_passes: FnCuptiProfilerHostGetNumOfPasses,
    cupti_profiler_host_evaluate_to_gpu_values: FnCuptiProfilerHostEvaluateToGpuValues,

    // CUPTI PM sampling functions
    cupti_pm_sampling_enable: FnCuptiPmSamplingEnable,
    cupti_pm_sampling_disable: FnCuptiPmSamplingDisable,
    cupti_pm_sampling_set_config: FnCuptiPmSamplingSetConfig,
    cupti_pm_sampling_start: FnCuptiPmSamplingStart,
    cupti_pm_sampling_stop: FnCuptiPmSamplingStop,
    cupti_pm_sampling_decode_data: FnCuptiPmSamplingDecodeData,
    cupti_pm_sampling_get_counter_availability: FnCuptiPmSamplingGetCounterAvailability,
    cupti_pm_sampling_get_counter_data_size: FnCuptiPmSamplingGetCounterDataSize,
    cupti_pm_sampling_counter_data_image_initialize: FnCuptiPmSamplingCounterDataImageInitialize,
    cupti_pm_sampling_get_counter_data_info: FnCuptiPmSamplingGetCounterDataInfo,
    cupti_pm_sampling_counter_data_get_sample_info: FnCuptiPmSamplingCounterDataGetSampleInfo,
}

// Safety: The function pointers are valid for the lifetime of the libraries,
// which are kept alive in the same struct. The functions themselves are thread-safe.
unsafe impl Send for CuptiLibraries {}
unsafe impl Sync for CuptiLibraries {}

static LIBRARIES: OnceLock<Result<CuptiLibraries, String>> = OnceLock::new();

impl CuptiLibraries {
    fn load() -> Result<Self, String> {
        // Try to load CUDA driver library
        let cuda_lib = Self::load_cuda()?;

        // Try to load CUPTI library
        let cupti_lib = Self::load_cupti()?;

        unsafe {
            // Load CUDA functions - extract function pointers immediately
            let cu_init: FnCuInit = *cuda_lib
                .get::<FnCuInit>(b"cuInit\0")
                .map_err(|e| format!("failed to load cuInit: {e}"))?;
            let cu_device_get: FnCuDeviceGet = *cuda_lib
                .get::<FnCuDeviceGet>(b"cuDeviceGet\0")
                .map_err(|e| format!("failed to load cuDeviceGet: {e}"))?;

            // Load CUPTI profiler functions - extract function pointers immediately
            let cupti_profiler_initialize: FnCuptiProfilerInitialize = *cupti_lib
                .get::<FnCuptiProfilerInitialize>(b"cuptiProfilerInitialize\0")
                .map_err(|e| format!("failed to load cuptiProfilerInitialize: {e}"))?;
            let cupti_profiler_deinitialize: FnCuptiProfilerDeInitialize = *cupti_lib
                .get::<FnCuptiProfilerDeInitialize>(b"cuptiProfilerDeInitialize\0")
                .map_err(|e| format!("failed to load cuptiProfilerDeInitialize: {e}"))?;
            let cupti_device_get_chip_name: FnCuptiDeviceGetChipName = *cupti_lib
                .get::<FnCuptiDeviceGetChipName>(b"cuptiDeviceGetChipName\0")
                .map_err(|e| format!("failed to load cuptiDeviceGetChipName: {e}"))?;
            let cupti_profiler_host_initialize: FnCuptiProfilerHostInitialize = *cupti_lib
                .get::<FnCuptiProfilerHostInitialize>(b"cuptiProfilerHostInitialize\0")
                .map_err(|e| format!("failed to load cuptiProfilerHostInitialize: {e}"))?;
            let cupti_profiler_host_deinitialize: FnCuptiProfilerHostDeinitialize = *cupti_lib
                .get::<FnCuptiProfilerHostDeinitialize>(b"cuptiProfilerHostDeinitialize\0")
                .map_err(|e| format!("failed to load cuptiProfilerHostDeinitialize: {e}"))?;
            let cupti_profiler_host_config_add_metrics: FnCuptiProfilerHostConfigAddMetrics =
                *cupti_lib
                    .get::<FnCuptiProfilerHostConfigAddMetrics>(
                        b"cuptiProfilerHostConfigAddMetrics\0",
                    )
                    .map_err(|e| {
                        format!("failed to load cuptiProfilerHostConfigAddMetrics: {e}")
                    })?;
            let cupti_profiler_host_get_config_image_size: FnCuptiProfilerHostGetConfigImageSize =
                *cupti_lib
                    .get::<FnCuptiProfilerHostGetConfigImageSize>(
                        b"cuptiProfilerHostGetConfigImageSize\0",
                    )
                    .map_err(|e| {
                        format!("failed to load cuptiProfilerHostGetConfigImageSize: {e}")
                    })?;
            let cupti_profiler_host_get_config_image: FnCuptiProfilerHostGetConfigImage =
                *cupti_lib
                    .get::<FnCuptiProfilerHostGetConfigImage>(b"cuptiProfilerHostGetConfigImage\0")
                    .map_err(|e| format!("failed to load cuptiProfilerHostGetConfigImage: {e}"))?;
            let cupti_profiler_host_get_num_of_passes: FnCuptiProfilerHostGetNumOfPasses =
                *cupti_lib
                    .get::<FnCuptiProfilerHostGetNumOfPasses>(b"cuptiProfilerHostGetNumOfPasses\0")
                    .map_err(|e| format!("failed to load cuptiProfilerHostGetNumOfPasses: {e}"))?;
            let cupti_profiler_host_evaluate_to_gpu_values: FnCuptiProfilerHostEvaluateToGpuValues =
                *cupti_lib
                    .get::<FnCuptiProfilerHostEvaluateToGpuValues>(
                        b"cuptiProfilerHostEvaluateToGpuValues\0",
                    )
                    .map_err(|e| {
                        format!("failed to load cuptiProfilerHostEvaluateToGpuValues: {e}")
                    })?;

            // Load CUPTI PM sampling functions - extract function pointers immediately
            let cupti_pm_sampling_enable: FnCuptiPmSamplingEnable = *cupti_lib
                .get::<FnCuptiPmSamplingEnable>(b"cuptiPmSamplingEnable\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingEnable: {e}"))?;
            let cupti_pm_sampling_disable: FnCuptiPmSamplingDisable = *cupti_lib
                .get::<FnCuptiPmSamplingDisable>(b"cuptiPmSamplingDisable\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingDisable: {e}"))?;
            let cupti_pm_sampling_set_config: FnCuptiPmSamplingSetConfig = *cupti_lib
                .get::<FnCuptiPmSamplingSetConfig>(b"cuptiPmSamplingSetConfig\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingSetConfig: {e}"))?;
            let cupti_pm_sampling_start: FnCuptiPmSamplingStart = *cupti_lib
                .get::<FnCuptiPmSamplingStart>(b"cuptiPmSamplingStart\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingStart: {e}"))?;
            let cupti_pm_sampling_stop: FnCuptiPmSamplingStop = *cupti_lib
                .get::<FnCuptiPmSamplingStop>(b"cuptiPmSamplingStop\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingStop: {e}"))?;
            let cupti_pm_sampling_decode_data: FnCuptiPmSamplingDecodeData = *cupti_lib
                .get::<FnCuptiPmSamplingDecodeData>(b"cuptiPmSamplingDecodeData\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingDecodeData: {e}"))?;
            let cupti_pm_sampling_get_counter_availability: FnCuptiPmSamplingGetCounterAvailability = *cupti_lib.get::<FnCuptiPmSamplingGetCounterAvailability>(b"cuptiPmSamplingGetCounterAvailability\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingGetCounterAvailability: {e}"))?;
            let cupti_pm_sampling_get_counter_data_size: FnCuptiPmSamplingGetCounterDataSize =
                *cupti_lib
                    .get::<FnCuptiPmSamplingGetCounterDataSize>(
                        b"cuptiPmSamplingGetCounterDataSize\0",
                    )
                    .map_err(|e| {
                        format!("failed to load cuptiPmSamplingGetCounterDataSize: {e}")
                    })?;
            let cupti_pm_sampling_counter_data_image_initialize: FnCuptiPmSamplingCounterDataImageInitialize = *cupti_lib.get::<FnCuptiPmSamplingCounterDataImageInitialize>(b"cuptiPmSamplingCounterDataImageInitialize\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingCounterDataImageInitialize: {e}"))?;
            let cupti_pm_sampling_get_counter_data_info: FnCuptiPmSamplingGetCounterDataInfo =
                *cupti_lib
                    .get::<FnCuptiPmSamplingGetCounterDataInfo>(
                        b"cuptiPmSamplingGetCounterDataInfo\0",
                    )
                    .map_err(|e| {
                        format!("failed to load cuptiPmSamplingGetCounterDataInfo: {e}")
                    })?;
            let cupti_pm_sampling_counter_data_get_sample_info: FnCuptiPmSamplingCounterDataGetSampleInfo = *cupti_lib.get::<FnCuptiPmSamplingCounterDataGetSampleInfo>(b"cuptiPmSamplingCounterDataGetSampleInfo\0")
                .map_err(|e| format!("failed to load cuptiPmSamplingCounterDataGetSampleInfo: {e}"))?;

            Ok(CuptiLibraries {
                _cuda: cuda_lib,
                _cupti: cupti_lib,
                cu_init,
                cu_device_get,
                cupti_profiler_initialize,
                cupti_profiler_deinitialize,
                cupti_device_get_chip_name,
                cupti_profiler_host_initialize,
                cupti_profiler_host_deinitialize,
                cupti_profiler_host_config_add_metrics,
                cupti_profiler_host_get_config_image_size,
                cupti_profiler_host_get_config_image,
                cupti_profiler_host_get_num_of_passes,
                cupti_profiler_host_evaluate_to_gpu_values,
                cupti_pm_sampling_enable,
                cupti_pm_sampling_disable,
                cupti_pm_sampling_set_config,
                cupti_pm_sampling_start,
                cupti_pm_sampling_stop,
                cupti_pm_sampling_decode_data,
                cupti_pm_sampling_get_counter_availability,
                cupti_pm_sampling_get_counter_data_size,
                cupti_pm_sampling_counter_data_image_initialize,
                cupti_pm_sampling_get_counter_data_info,
                cupti_pm_sampling_counter_data_get_sample_info,
            })
        }
    }

    fn load_cuda() -> Result<Library, String> {
        // Try common CUDA driver library paths
        let candidates = [
            "libcuda.so.1",
            "libcuda.so",
            "/usr/lib/x86_64-linux-gnu/libcuda.so.1",
            "/usr/lib/x86_64-linux-gnu/libcuda.so",
            "/usr/local/cuda/lib64/stubs/libcuda.so",
        ];

        for path in candidates {
            if let Ok(lib) = unsafe { Library::new(path) } {
                return Ok(lib);
            }
        }

        Err("CUDA driver library (libcuda.so) not found".to_string())
    }

    fn load_cupti() -> Result<Library, String> {
        // Try common CUPTI library paths
        // The library name varies by CUDA version
        let candidates = [
            "libcupti.so.12",
            "libcupti.so.13",
            "libcupti.so.11",
            "libcupti.so",
            "/usr/local/cuda/extras/CUPTI/lib64/libcupti.so",
            "/usr/local/cuda/lib64/libcupti.so",
        ];

        for path in candidates {
            if let Ok(lib) = unsafe { Library::new(path) } {
                return Ok(lib);
            }
        }

        Err("CUPTI library (libcupti.so) not found. Ensure CUDA toolkit is installed.".to_string())
    }
}

fn get_libs() -> Result<&'static CuptiLibraries, &'static str> {
    LIBRARIES
        .get_or_init(|| CuptiLibraries::load())
        .as_ref()
        .map_err(|e| e.as_str())
}

// =============================================================================
// Public API - Safe Wrappers
// =============================================================================

/// Initialize the CUPTI libraries at runtime.
///
/// This must be called before any other CUPTI functions. Returns an error
/// if CUDA or CUPTI libraries cannot be loaded.
pub fn load_libraries() -> Result<(), &'static str> {
    get_libs().map(|_| ())
}

/// Check if CUPTI libraries are available.
pub fn is_available() -> bool {
    get_libs().is_ok()
}

// =============================================================================
// Public API - Raw Function Calls
// =============================================================================

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuInit(flags: c_uint) -> CUresult {
    match get_libs() {
        Ok(libs) => (libs.cu_init)(flags),
        Err(_) => 999, // CUDA_ERROR_UNKNOWN
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuDeviceGet(device: *mut CUdevice, ordinal: c_int) -> CUresult {
    match get_libs() {
        Ok(libs) => (libs.cu_device_get)(device, ordinal),
        Err(_) => 999,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerInitialize(
    params: *mut CUpti_Profiler_Initialize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_initialize)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerDeInitialize(
    params: *mut CUpti_Profiler_DeInitialize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_deinitialize)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiDeviceGetChipName(params: *mut CUpti_Device_GetChipName_Params) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_device_get_chip_name)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostInitialize(
    params: *mut CUpti_Profiler_Host_Initialize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_initialize)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostDeinitialize(
    params: *mut CUpti_Profiler_Host_Deinitialize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_deinitialize)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostConfigAddMetrics(
    params: *mut CUpti_Profiler_Host_ConfigAddMetrics_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_config_add_metrics)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostGetConfigImageSize(
    params: *mut CUpti_Profiler_Host_GetConfigImageSize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_get_config_image_size)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostGetConfigImage(
    params: *mut CUpti_Profiler_Host_GetConfigImage_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_get_config_image)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostGetNumOfPasses(
    params: *mut CUpti_Profiler_Host_GetNumOfPasses_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_get_num_of_passes)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiProfilerHostEvaluateToGpuValues(
    params: *mut CUpti_Profiler_Host_EvaluateToGpuValues_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_profiler_host_evaluate_to_gpu_values)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingEnable(params: *mut CUpti_PmSampling_Enable_Params) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_enable)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingDisable(params: *mut CUpti_PmSampling_Disable_Params) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_disable)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingSetConfig(
    params: *mut CUpti_PmSampling_SetConfig_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_set_config)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingStart(params: *mut CUpti_PmSampling_Start_Params) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_start)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingStop(params: *mut CUpti_PmSampling_Stop_Params) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_stop)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingDecodeData(
    params: *mut CUpti_PmSampling_DecodeData_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_decode_data)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingGetCounterAvailability(
    params: *mut CUpti_PmSampling_GetCounterAvailability_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_get_counter_availability)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingGetCounterDataSize(
    params: *mut CUpti_PmSampling_GetCounterDataSize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_get_counter_data_size)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingCounterDataImageInitialize(
    params: *mut CUpti_PmSampling_CounterDataImage_Initialize_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_counter_data_image_initialize)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingGetCounterDataInfo(
    params: *mut CUpti_PmSampling_GetCounterDataInfo_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_get_counter_data_info)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}

/// # Safety
/// Caller must ensure params is valid.
pub unsafe fn cuptiPmSamplingCounterDataGetSampleInfo(
    params: *mut CUpti_PmSampling_CounterData_GetSampleInfo_Params,
) -> CUptiResult {
    match get_libs() {
        Ok(libs) => (libs.cupti_pm_sampling_counter_data_get_sample_info)(params),
        Err(_) => CUPTI_ERROR_UNKNOWN,
    }
}
