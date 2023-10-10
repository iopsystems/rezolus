use std::io;

use lazy_static::lazy_static;
use systeminfo::hwinfo::HwInfo;

lazy_static! {
    pub static ref HWINFO: io::Result<HwInfo> = HwInfo::new() //
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e));
}

pub fn hardware_info() -> std::result::Result<&'static HwInfo, &'static std::io::Error> {
    HWINFO.as_ref()
}
