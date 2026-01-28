use std::fmt;
use std::num::NonZeroU32;

use cupti_sys::*;

macro_rules! error_enum
{
    {
        $( #[$attr:meta] )*
        pub enum $name:ident {
            $(
                $( #[$fattr:meta] )*
                $variant:ident = $value:expr
            ),* $(,)?
        }
    } => {
        $(#[$attr])*
        pub struct $name(NonZeroU32);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(
                $( #[$fattr] )*
                pub const $variant: Self = Self(NonZeroU32::new($value).unwrap());
            )*
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(match *self {
                    $( Self::$variant => stringify!($variant), )*
                    _ => return f.debug_tuple(stringify!($name)).field(&self.0).finish()
                })
            }
        }
    }
}

error_enum! {
    /// Errors that can be returned from CUPTI functions.
    ///
    /// This is meant to be usable like an enum, but it is possible for CUPTI
    /// functions to return error codes not listed here (either due to internal
    /// errors or a new version).
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub enum Error {
        /// One or more of the parameters is invalid.
        InvalidParameter = CUPTI_ERROR_INVALID_PARAMETER,

        /// The device does not correspond to a valid CUDA device.
        InvalidDevice = CUPTI_ERROR_INVALID_DEVICE,

        /// The context is NULL or not valid.
        InvalidContext = CUPTI_ERROR_INVALID_CONTEXT,

        /// The event domain id is invalid.
        InvalidEventDomainID = CUPTI_ERROR_INVALID_EVENT_DOMAIN_ID,

        /// The event id is invalid.
        InvalidEventID = CUPTI_ERROR_INVALID_EVENT_ID,

        /// The event name is invalid.
        InvalidEventName = CUPTI_ERROR_INVALID_EVENT_NAME,

        /// The current operation cannot be performed due to a dependency on
        /// other features.
        InvalidOperation = CUPTI_ERROR_INVALID_OPERATION,

        /// Unable to allocate enough memory to perform the requested operation.
        OutOfMemory = CUPTI_ERROR_OUT_OF_MEMORY,

        /// An error occurred on the performance monitoring hardware.
        Hardware = CUPTI_ERROR_HARDWARE,

        /// The output buffer size is not sufficient to return all requested data.
        ParameterSizeNotSufficient = CUPTI_ERROR_PARAMETER_SIZE_NOT_SUFFICIENT,

        /// CUPTI is unable to initialize its connection to the CUDA driver.
        NotInitialized = CUPTI_ERROR_NOT_INITIALIZED,

        /// The current operation is not compatible with the current state of
        /// the object.
        NotCompatible = CUPTI_ERROR_NOT_COMPATIBLE,

        /// The attempted operation is not supported on the current system or
        /// device.
        NotSupported = CUPTI_ERROR_NOT_SUPPORTED,

        /// The metric name is invalid.
        InvalidMetricName = CUPTI_ERROR_INVALID_METRIC_NAME,

        /// User doesn't have sufficient privileges which are required to start
        /// the profiling session.
        InsufficientPrivileges = CUPTI_ERROR_INSUFFICIENT_PRIVILEGES,

        /// An unknown internal error has occurred.
        Unknown = CUPTI_ERROR_UNKNOWN,
    }
}

impl Error {
    /// Create a new error object from an error code.
    pub const fn new(code: u32) -> Option<Self> {
        match NonZeroU32::new(code) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// Create an error result directly from an error code.
    pub const fn result(code: u32) -> Result<(), Error> {
        match Self::new(code) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Get the underlying error code for this error.
    pub const fn code(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CUPTI error {:?} (code {})", self, self.code())
    }
}

impl std::error::Error for Error {}
