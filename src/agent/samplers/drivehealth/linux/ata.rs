//! SATA temperature via `SG_IO` ATA PASS-THROUGH(16) — the module-free path
//! (`smartctl`/`hddtemp` use the same mechanism). We issue the read-only ATA
//! `SMART READ DATA` command (opcode `0xB0`, feature `0xD0`) over the block
//! device and parse the returned 512-byte attribute table for the temperature
//! attribute. No `drivetemp` module is involved.
//!
//! The parser is pure and unit-tested against fixture bytes; the ioctl glue is
//! thin unsafe code, hardware-verified against `smartctl -A`.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

const SG_IO: libc::c_ulong = 0x2285;
const SG_DXFER_FROM_DEV: libc::c_int = -3;

/// Mirror of the kernel `sg_io_hdr` (`<scsi/sg.h>`). `#[repr(C)]` reproduces the
/// exact field layout/padding the `SG_IO` ioctl expects.
#[repr(C)]
struct SgIoHdr {
    interface_id: libc::c_int,
    dxfer_direction: libc::c_int,
    cmd_len: libc::c_uchar,
    mx_sb_len: libc::c_uchar,
    iovec_count: libc::c_ushort,
    dxfer_len: libc::c_uint,
    dxferp: *mut libc::c_void,
    cmdp: *const libc::c_uchar,
    sbp: *mut libc::c_uchar,
    timeout: libc::c_uint,
    flags: libc::c_uint,
    pack_id: libc::c_int,
    usr_ptr: *mut libc::c_void,
    status: libc::c_uchar,
    masked_status: libc::c_uchar,
    msg_status: libc::c_uchar,
    sb_len_wr: libc::c_uchar,
    host_status: libc::c_ushort,
    driver_status: libc::c_ushort,
    resid: libc::c_int,
    duration: libc::c_uint,
    info: libc::c_uint,
}

/// Parse an ATA `SMART READ DATA` response (512 bytes) and return the drive
/// temperature in °C. The layout: 2-byte revision, then 30 × 12-byte attribute
/// entries starting at offset 2. Each entry is `[id(1), flags(2), value(1),
/// worst(1), raw(6), reserved(1)]`; for a temperature attribute the low raw byte
/// (entry offset 5) is the temperature in °C. Prefer attribute 194
/// (`Temperature_Celsius`); fall back to 190 (`Airflow_Temperature_Cel`).
/// Returns `None` if the buffer is too short or neither attribute is present.
pub fn parse_smart_temperature(buf: &[u8]) -> Option<i64> {
    const ENTRIES: usize = 30;
    const ENTRY_LEN: usize = 12;
    const TABLE_START: usize = 2;

    if buf.len() < TABLE_START + ENTRIES * ENTRY_LEN {
        return None;
    }

    let mut fallback = None;
    for i in 0..ENTRIES {
        let base = TABLE_START + i * ENTRY_LEN;
        let id = buf[base];
        let raw_low = buf[base + 5] as i64;
        match id {
            194 => return Some(raw_low),
            190 => fallback = Some(raw_low),
            _ => {}
        }
    }
    fallback
}

/// Issue `SMART READ DATA` to the drive at `dev` (e.g. `/dev/sda`) via
/// `SG_IO` ATA PASS-THROUGH(16) and return its temperature in °C, or `None` on
/// any open/ioctl/parse failure.
pub fn read_temperature(dev: &Path) -> Option<i64> {
    let cpath = std::ffi::CString::new(dev.as_os_str().as_bytes()).ok()?;
    // Read-only, non-blocking open — never mutates the device.
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return None;
    }
    let result = smart_read_data(fd).and_then(|buf| parse_smart_temperature(&buf));
    unsafe { libc::close(fd) };
    result
}

/// Issue the read-only ATA `SMART READ DATA` command via `SG_IO` ATA
/// PASS-THROUGH(16) and return the 512-byte attribute table.
fn smart_read_data(fd: RawFd) -> Option<[u8; 512]> {
    // ATA PASS-THROUGH(16) wrapping SMART READ DATA (0xB0, feature 0xD0):
    // PIO Data-In, 1 sector from device, SMART signature 0x4F/0xC2. Read-only.
    let cdb: [u8; 16] = [
        0x85, 0x08, 0x0e, 0x00, 0xd0, 0x00, 0x01, 0x00, 0x00, 0x00, 0x4f, 0x00, 0xc2, 0x00, 0xb0,
        0x00,
    ];
    let mut data = [0u8; 512];
    let mut sense = [0u8; 32];

    let mut hdr: SgIoHdr = unsafe { std::mem::zeroed() };
    hdr.interface_id = b'S' as libc::c_int;
    hdr.dxfer_direction = SG_DXFER_FROM_DEV;
    hdr.cmd_len = cdb.len() as libc::c_uchar;
    hdr.mx_sb_len = sense.len() as libc::c_uchar;
    hdr.dxfer_len = data.len() as libc::c_uint;
    hdr.dxferp = data.as_mut_ptr() as *mut libc::c_void;
    hdr.cmdp = cdb.as_ptr();
    hdr.sbp = sense.as_mut_ptr();
    hdr.timeout = 5_000; // ms

    let rc = unsafe { libc::ioctl(fd, SG_IO, &mut hdr as *mut SgIoHdr) };
    if rc < 0 {
        return None;
    }
    // Drop the result unless the transport reports a clean completion.
    if hdr.status != 0 || hdr.host_status != 0 {
        return None;
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 512-byte SMART READ DATA buffer with the given attributes placed
    /// in successive entries. Each `(id, raw_low)` becomes one 12-byte entry.
    fn smart_buf(attrs: &[(u8, u8)]) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        buf[0] = 0x10; // revision, arbitrary
        for (i, (id, raw_low)) in attrs.iter().enumerate() {
            let base = 2 + i * 12;
            buf[base] = *id;
            buf[base + 5] = *raw_low; // raw value low byte = temperature
        }
        buf
    }

    #[test]
    fn reads_temperature_celsius_attribute_194() {
        // id 194 present among others -> its raw low byte is the temperature.
        let buf = smart_buf(&[(5, 100), (194, 38), (9, 12)]);
        assert_eq!(parse_smart_temperature(&buf), Some(38));
    }

    #[test]
    fn falls_back_to_airflow_190_when_194_absent() {
        let buf = smart_buf(&[(5, 100), (190, 41)]);
        assert_eq!(parse_smart_temperature(&buf), Some(41));
    }

    #[test]
    fn prefers_194_over_190() {
        let buf = smart_buf(&[(190, 41), (194, 38)]);
        assert_eq!(parse_smart_temperature(&buf), Some(38));
    }

    #[test]
    fn none_when_no_temperature_attribute() {
        let buf = smart_buf(&[(5, 100), (9, 12), (12, 7)]);
        assert_eq!(parse_smart_temperature(&buf), None);
    }

    #[test]
    fn none_when_buffer_too_short() {
        assert_eq!(parse_smart_temperature(&[0u8; 8]), None);
    }

    /// Hardware smoke test — requires root and a real SATA drive. Ignored by
    /// default; run explicitly against a known drive, e.g.:
    ///   cargo test --bin rezolus --no-run
    ///   sudo ./target/debug/deps/rezolus-* ata::tests::hardware -- --ignored --nocapture
    /// then compare the printed value to `smartctl -A /dev/sdX`.
    #[test]
    #[ignore]
    fn hardware_read_dev_sda() {
        let t = read_temperature(Path::new("/dev/sda"));
        println!("/dev/sda temperature = {t:?} C");
        let c = t.expect("expected a temperature from /dev/sda");
        assert!(
            (10..=90).contains(&c),
            "temperature {c} out of plausible range"
        );
    }
}
