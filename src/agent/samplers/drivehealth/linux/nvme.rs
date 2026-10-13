//! NVMe temperature via `NVME_IOCTL_ADMIN_CMD` — Get Log Page 0x02 (SMART /
//! Health Information), the module-free path (the `nvme` driver is already bound
//! to the device). We issue the read-only Get Log Page admin command and decode
//! the Composite Temperature plus the monotonic thermal-throttle counters
//! (Warning/Critical Composite Temperature Time, host thermal-management
//! transitions/time). The remaining SMART-log health (wear/spare/errors) is a
//! later phase off the same read. On an Amazon EBS or instance store device
//! the same open also reads the EC2 statistics page 0xD0 ([`super::ec2`]).
//!
//! The parser is pure, unit-tested and shared with macOS (`../nvme_log.rs`);
//! the ioctl glue is thin unsafe code.
//! Note: no NVMe drive was available on the development host, so the ioctl path
//! is fixture-verified only — hardware validation is a documented reopen
//! condition in the journal entry.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

use super::super::nvme_log::parse_health;
pub use super::super::nvme_log::NvmeHealth;
use super::ec2::{self, Ec2Stats, Ec2Storage};

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

/// The namespace the EC2 statistics page is read for: the nsid `ebsnvme`, the
/// EBS CSI driver and the CloudWatch agent send, the last also on the
/// controller device. nvme-cli sends 1 for EBS and, from a controller device,
/// `NVME_NSID_ALL` for instance store.
const EC2_STATS_NSID: u32 = 1;

/// Issue Get Log Page 0x02 to the NVMe controller at `dev` (e.g. `/dev/nvme0`)
/// and return its decoded health (temperature + thermal-throttle counters), and
/// for an EC2 device (`ec2`) the EC2 statistics page 0xD0 as well. Each is
/// `None` when its read or decode fails. One open serves both reads.
pub fn read_health(dev: &Path, ec2: Option<Ec2Storage>) -> (Option<NvmeHealth>, Option<Ec2Stats>) {
    let Some(cpath) = std::ffi::CString::new(dev.as_os_str().as_bytes()).ok() else {
        return (None, None);
    };
    // Read-only, non-blocking open — never mutates the device.
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return (None, None);
    }
    let mut smart = [0u8; 512];
    let health = get_log_page(fd, NVME_LOG_SMART, 0xffff_ffff, &mut smart)
        .then(|| parse_health(&smart))
        .flatten();
    let stats = ec2.and_then(|storage| {
        let mut page = vec![0u8; ec2::LOG_PAGE_LEN];
        get_log_page(fd, ec2::LOG_PAGE, EC2_STATS_NSID, &mut page)
            .then(|| ec2::parse(&page, storage))
            .flatten()
    });
    unsafe { libc::close(fd) };
    (health, stats)
}

/// Fetch log page `lid` for namespace `nsid` into `data` via the admin
/// passthrough ioctl. `data`'s length must be a whole number of dwords.
fn get_log_page(fd: RawFd, lid: u32, nsid: u32, data: &mut [u8]) -> bool {
    // numd = (dwords - 1), lower 16 bits in cdw10[31:16], upper in cdw11[15:0]:
    // 512 bytes = 128 dwords -> 127; 4096 bytes = 1024 dwords -> 1023.
    let numd: u32 = (data.len() as u32 / 4) - 1;

    let cmd = NvmePassthruCmd {
        opcode: NVME_ADMIN_GET_LOG_PAGE,
        nsid,
        addr: data.as_mut_ptr() as u64,
        data_len: data.len() as u32,
        cdw10: lid | ((numd & 0xffff) << 16),
        cdw11: numd >> 16,
        timeout_ms: 5_000,
        ..Default::default()
    };

    let rc = unsafe { libc::ioctl(fd, NVME_IOCTL_ADMIN_CMD, &cmd as *const NvmePassthruCmd) };
    rc == 0
}
