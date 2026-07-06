//! NVMe temperature via `NVME_IOCTL_ADMIN_CMD` — Get Log Page 0x02 (SMART /
//! Health Information), the module-free path (the `nvme` driver is already bound
//! to the device). We issue the read-only Get Log Page admin command and read
//! the Composite Temperature field. This is the same log page Phase 2 will use
//! for wear / spare / critical-warning health.
//!
//! The parser is pure and unit-tested; the ioctl glue is thin unsafe code.
//! Note: no NVMe drive was available on the development host, so the ioctl path
//! is fixture-verified only — hardware validation is a documented reopen
//! condition in the journal entry.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

/// Parse an NVMe SMART / Health log page (0x02). Composite Temperature is a
/// little-endian `u16` at bytes 1..3, in Kelvin. Returns °C, or `None` if the
/// buffer is too short or the field is 0 (not reported).
pub fn parse_smart_temperature(buf: &[u8]) -> Option<i64> {
    if buf.len() < 3 {
        return None;
    }
    let kelvin = u16::from_le_bytes([buf[1], buf[2]]) as i64;
    if kelvin == 0 {
        return None; // field not reported
    }
    Some(kelvin - 273)
}

// _IOWR('N', 0x41, struct nvme_passthru_cmd), struct size 72 on Linux.
const NVME_IOCTL_ADMIN_CMD: libc::c_ulong = 0xC048_4E41;
const NVME_ADMIN_GET_LOG_PAGE: u8 = 0x02;
const NVME_LOG_SMART: u32 = 0x02;

/// Mirror of the kernel `struct nvme_passthru_cmd` (`<linux/nvme_ioctl.h>`).
#[repr(C)]
#[derive(Default)]
struct NvmePassthruCmd {
    opcode: u8,
    flags: u8,
    rsvd1: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

/// Issue Get Log Page 0x02 to the NVMe controller at `dev` (e.g. `/dev/nvme0`)
/// and return its Composite Temperature in °C, or `None` on failure.
pub fn read_temperature(dev: &Path) -> Option<i64> {
    let cpath = std::ffi::CString::new(dev.as_os_str().as_bytes()).ok()?;
    // Read-only, non-blocking open — never mutates the device.
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return None;
    }
    let result = get_smart_log(fd).and_then(|buf| parse_smart_temperature(&buf));
    unsafe { libc::close(fd) };
    result
}

/// Fetch the 512-byte SMART/Health log page via the admin passthrough ioctl.
fn get_smart_log(fd: RawFd) -> Option<[u8; 512]> {
    let mut data = [0u8; 512];
    // numd = (dwords - 1); 512 bytes = 128 dwords -> numd 127 in cdw10[31:16].
    let numd: u32 = (data.len() as u32 / 4) - 1;

    let cmd = NvmePassthruCmd {
        opcode: NVME_ADMIN_GET_LOG_PAGE,
        nsid: 0xffff_ffff,
        addr: data.as_mut_ptr() as u64,
        data_len: data.len() as u32,
        cdw10: NVME_LOG_SMART | (numd << 16),
        timeout_ms: 5_000,
        ..Default::default()
    };

    let rc = unsafe { libc::ioctl(fd, NVME_IOCTL_ADMIN_CMD, &cmd as *const NvmePassthruCmd) };
    if rc != 0 {
        return None;
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SMART log buffer with Composite Temperature set to `kelvin`.
    fn smart_log(kelvin: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        let [lo, hi] = kelvin.to_le_bytes();
        buf[1] = lo;
        buf[2] = hi;
        buf
    }

    #[test]
    fn converts_kelvin_to_celsius() {
        // 311 K = 38 C.
        assert_eq!(parse_smart_temperature(&smart_log(311)), Some(38));
    }

    #[test]
    fn zero_field_is_none() {
        assert_eq!(parse_smart_temperature(&smart_log(0)), None);
    }

    #[test]
    fn too_short_is_none() {
        assert_eq!(parse_smart_temperature(&[0u8; 2]), None);
    }
}
