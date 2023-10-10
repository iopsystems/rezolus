//! Gather a comprehensive description of the current system.
//!

use std::io;

#[macro_use]
extern crate serde;

pub mod hwinfo;

/// Read the [`SystemInfo`] for the current system.
pub fn systeminfo() -> io::Result<SystemInfo> {
    SystemInfo::new()
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hwinfo: crate::hwinfo::HwInfo,
}

impl SystemInfo {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            hwinfo: crate::hwinfo::HwInfo::new()?,
        })
    }
}
