//! Gather a comprehensive description of the current system.
//!

#[macro_use]
extern crate serde;

mod error;
pub mod hwinfo;

pub use crate::error::{Error, Result};

/// Read the [`SystemInfo`] for the current system.
pub fn systeminfo() -> Result<SystemInfo> {
    SystemInfo::new()
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hwinfo: crate::hwinfo::HwInfo,
}

impl SystemInfo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            hwinfo: crate::hwinfo::HwInfo::new()?,
        })
    }
}
