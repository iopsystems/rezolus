//! Gather a comprehensive description of the current system.
//! 

use std::io;

#[macro_use]
extern crate serde;

/// Read the [`SystemInfo`] for the current system.
pub fn systeminfo() -> io::Result<SystemInfo> {
    SystemInfo::new()
}

#[non_exhaustive]
#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct SystemInfo {

}

impl SystemInfo {
    pub fn new() -> io::Result<Self> {
        todo!()
    }
}
