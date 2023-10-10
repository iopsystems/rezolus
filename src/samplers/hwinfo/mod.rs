use std::io;

use lazy_static::lazy_static;
use systeminfo::hwinfo::HwInfo;

lazy_static! {
    pub static ref HWINFO: io::Result<HwInfo> = HwInfo::new();
}

pub fn hardware_info() -> std::result::Result<&'static HwInfo, &'static std::io::Error> {
    HWINFO.as_ref()
}
