//! Block IO counters that only the macOS storage driver reports, read in the
//! same sweep as `blockio_operations` and `blockio_bytes`.

use metriken::*;

#[metric(
    name = "blockio_retries",
    description = "Retries the storage driver performed for block IO (IOBlockStorageDriver \"Retries\")",
    metadata = { op = "read", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_READ_RETRIES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_retries",
    description = "Retries the storage driver performed for block IO (IOBlockStorageDriver \"Retries\")",
    metadata = { op = "write", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_WRITE_RETRIES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_service_time",
    description = "Nanoseconds the storage driver reports spending on block IO (IOBlockStorageDriver \"Total Time\", which its header describes as the time spent performing reads or writes)",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_READ_SERVICE_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_service_time",
    description = "Nanoseconds the storage driver reports spending on block IO (IOBlockStorageDriver \"Total Time\", which its header describes as the time spent performing reads or writes)",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_WRITE_SERVICE_TIME: LazyCounter = LazyCounter::new(Counter::default);

// Not `blockio_errors`: that counts terminal failures by class, and the
// driver's count is unclassified and not documented as terminal.
#[metric(
    name = "blockio_driver_errors",
    description = "Errors the storage driver reports for block IO (IOBlockStorageDriver \"Errors\"). The driver does not classify them, and its header does not say whether a request that succeeded on retry is counted.",
    metadata = { op = "read", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_DRIVER_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_driver_errors",
    description = "Errors the storage driver reports for block IO (IOBlockStorageDriver \"Errors\"). The driver does not classify them, and its header does not say whether a request that succeeded on retry is counted.",
    metadata = { op = "write", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_DRIVER_ERRORS: LazyCounter = LazyCounter::new(Counter::default);
