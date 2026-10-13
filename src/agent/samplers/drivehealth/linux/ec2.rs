//! Amazon EC2 NVMe detailed performance statistics: vendor-specific log page
//! 0xD0, served by EBS volumes and NVMe instance store volumes on Nitro
//! instances. It reports, among other things, how long the device's I/O
//! demand exceeded an IOPS or throughput limit, which is how EC2 storage
//! throttling shows up from inside the instance.
//!
//! References:
//! - EBS: <https://docs.aws.amazon.com/ebs/latest/userguide/nvme-detailed-performance-stats.html>
//!   (statistic names, microsecond units, at most one read per second is
//!   useful, Nitro only).
//! - Instance store: <https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/nvme-detailed-performance-stats.html>
//! - Page layout and magic values: nvme-cli's `amzn` plugin,
//!   <https://github.com/linux-nvme/nvme-cli/blob/master/plugins/amzn/amzn-nvme.c>
//!   (`struct amzn_latency_log_page_base`, `AMZN_NVME_STATS_LOGPAGE_ID`,
//!   `AMZN_NVME_EBS_STATS_MAGIC`, `AMZN_NVME_LOCAL_STORAGE_STATS_MAGIC`), and
//!   AWS's `ebsnvme` script,
//!   <https://github.com/amazonlinux/amazon-ec2-utils/blob/main/ebsnvme>
//!   (`nvme_get_amzn_stats_logpage`), which agree on the offsets used here.
//!
//! Layout of the first 96 bytes, little-endian, packed:
//!
//! | offset | type | field |
//! |--------|------|-------|
//! | 0  | u32 | magic: `0x3C23B510` EBS, `0xEC2C0D7E` instance store |
//! | 4  | u32 | version (nvme-cli; `ebsnvme` treats it as reserved) |
//! | 8-48 | u64 x 6 | total read/write ops, bytes, time (us) |
//! | 56 | u64 | `ebs_volume_performance_exceeded_iops` (us) |
//! | 64 | u64 | `ebs_volume_performance_exceeded_tp` (us) |
//! | 72 | u64 | `ec2_instance_performance_exceeded_iops` (us) |
//! | 80 | u64 | `ec2_instance_performance_exceeded_tp` (us) |
//! | 88 | u64 | `volume_queue_length` (point in time) |
//!
//! Latency histograms follow at offset 512; they are not decoded here.
//!
//! On instance store, the pair at 72/80 is the volume's own limit
//! (`instance_store_volume_performance_exceeded_iops`/`_tp` on AWS's instance
//! store page) and 56/64 are not applicable: the CloudWatch agent's instance
//! store scraper,
//! <https://github.com/aws/amazon-cloudwatch-agent/blob/main/receiver/awsnvmereceiver/internal/instancestore/scraper.go>,
//! marks 56/64 not applicable and publishes 72/80, and nvme-cli's text output
//! prints 72/80 as "EC2 Instance Local Storage Performance Exceeded". This
//! decoder has not been checked against a real EBS or instance store device.

/// The vendor-specific Get Log Page identifier for the statistics.
pub const LOG_PAGE: u32 = 0xD0;

/// Bytes read: the page's base section (`sizeof(struct
/// amzn_latency_log_page_base)` in nvme-cli), which is what nvme-cli reads
/// from an EBS volume. Instance store appends per-I/O-size histograms after
/// it, which are not read.
pub const LOG_PAGE_LEN: usize = 4096;

const EBS_MAGIC: u32 = 0x3C23_B510;
const INSTANCE_STORE_MAGIC: u32 = 0xEC2C_0D7E;

/// Identify Controller model numbers, as sysfs reports them in
/// `/sys/class/nvme/<ctrl>/model`. `ebsnvme` matches the EBS model exactly;
/// nvme-cli matches the instance store model by prefix.
const EBS_MODEL: &str = "Amazon Elastic Block Store";
const INSTANCE_STORE_MODEL_PREFIX: &str = "Amazon EC2 NVMe Instance Storage";

/// Which kind of EC2 NVMe device serves the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ec2Storage {
    Ebs,
    InstanceStore,
}

impl Ec2Storage {
    /// The kind for an NVMe controller model, or `None` for any other drive,
    /// which is never sent the vendor-specific command.
    pub fn from_model(model: &str) -> Option<Self> {
        let model = model.trim();
        if model == EBS_MODEL {
            Some(Self::Ebs)
        } else if model.starts_with(INSTANCE_STORE_MODEL_PREFIX) {
            Some(Self::InstanceStore)
        } else {
            None
        }
    }
}

/// Time in microseconds, as the device reports it, that IOPS and throughput
/// demand exceeded one limit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Exceeded {
    pub iops_us: u64,
    pub throughput_us: u64,
}

/// The decoded throttling fields of one device's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ec2Stats {
    Ebs {
        /// The volume's provisioned IOPS and throughput (offsets 56/64).
        volume: Exceeded,
        /// The attached instance's EBS IOPS and throughput limits (72/80).
        instance: Exceeded,
        queue_length: u64,
    },
    InstanceStore {
        /// The volume's maximum IOPS and throughput (72/80; 56/64 are not
        /// applicable on instance store).
        volume: Exceeded,
        queue_length: u64,
    },
}

/// Decode the page read from a device of kind `storage`. Returns `None` if the
/// buffer is short or its magic does not match `storage`.
pub fn parse(buf: &[u8], storage: Ec2Storage) -> Option<Ec2Stats> {
    if buf.len() < 96 {
        return None;
    }
    let u32le = |o: usize| u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
    let u64le = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
    let pair = |o: usize| Exceeded {
        iops_us: u64le(o),
        throughput_us: u64le(o + 8),
    };

    match storage {
        Ec2Storage::Ebs if u32le(0) == EBS_MAGIC => Some(Ec2Stats::Ebs {
            volume: pair(56),
            instance: pair(72),
            queue_length: u64le(88),
        }),
        Ec2Storage::InstanceStore if u32le(0) == INSTANCE_STORE_MAGIC => {
            Some(Ec2Stats::InstanceStore {
                volume: pair(72),
                queue_length: u64le(88),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page with every u64 field from offset 8 to 88 set to its own offset,
    /// so a decoder reading the wrong offset gives the wrong value.
    fn page(magic: u32) -> Vec<u8> {
        let mut buf = vec![0u8; LOG_PAGE_LEN];
        buf[0..4].copy_from_slice(&magic.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        for offset in (8..=88).step_by(8) {
            buf[offset..offset + 8].copy_from_slice(&(offset as u64).to_le_bytes());
        }
        buf
    }

    #[test]
    fn decodes_an_ebs_page_at_the_documented_offsets() {
        assert_eq!(
            parse(&page(EBS_MAGIC), Ec2Storage::Ebs),
            Some(Ec2Stats::Ebs {
                volume: Exceeded {
                    iops_us: 56,
                    throughput_us: 64,
                },
                instance: Exceeded {
                    iops_us: 72,
                    throughput_us: 80,
                },
                queue_length: 88,
            })
        );
    }

    #[test]
    fn an_instance_store_page_reads_its_volume_limit_at_72_and_80() {
        assert_eq!(
            parse(&page(INSTANCE_STORE_MAGIC), Ec2Storage::InstanceStore),
            Some(Ec2Stats::InstanceStore {
                volume: Exceeded {
                    iops_us: 72,
                    throughput_us: 80,
                },
                queue_length: 88,
            })
        );
    }

    #[test]
    fn a_wrong_magic_or_short_page_is_refused() {
        assert_eq!(parse(&page(INSTANCE_STORE_MAGIC), Ec2Storage::Ebs), None);
        assert_eq!(parse(&page(EBS_MAGIC), Ec2Storage::InstanceStore), None);
        assert_eq!(parse(&page(0), Ec2Storage::Ebs), None);
        assert_eq!(parse(&page(EBS_MAGIC)[..95], Ec2Storage::Ebs), None);
    }

    #[test]
    fn only_amazon_models_are_ec2_storage() {
        assert_eq!(
            Ec2Storage::from_model("Amazon Elastic Block Store"),
            Some(Ec2Storage::Ebs)
        );
        assert_eq!(
            Ec2Storage::from_model("Amazon EC2 NVMe Instance Storage"),
            Some(Ec2Storage::InstanceStore)
        );
        assert_eq!(Ec2Storage::from_model("Samsung SSD 990 PRO"), None);
        assert_eq!(Ec2Storage::from_model(""), None);
    }
}
