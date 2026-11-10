//! Collects AWS Elastic Fabric Adapter (EFA) counters from sysfs.
//!
//! An EFA device appears under `/sys/class/infiniband` (`rdmap*`), with its
//! driver at `device/driver` named `efa`. Each port's
//! `ports/<n>/hw_counters/` holds its traffic, RDMA and retransmission
//! counters; the device's own `hw_counters/` holds admin command and verbs
//! error counters. These have no BPF or perf source, so each read parses a
//! sysfs file (principle 15's exception). EFA has no `ports/<n>/counters/`
//! (the InfiniBand port counters other RDMA drivers expose).
//!
//! The set of files differs by driver version: kernel 6.18's driver has the
//! `retrans_*`, `unresponsive_remote_events` and
//! `impaired_remote_conn_events` port counters, and Amazon's out-of-tree
//! driver adds the `alloc_mr_err` and `get_dma_mr_err` device counters. Each
//! file is opened if it exists, and a counter with no file publishes nothing.
//!
//! ## Cadence
//!
//! Reading a port's counters makes the driver send four or five admin commands
//! to the device (`efa_fill_port_stats`); the RDMA core then serves every file
//! of that port from the result for `hw_counters/lifespan` (10 ms). On an
//! im4gn.16xlarge sending about 55 Gbit/s over EFA, the first file read took
//! 1.8 ms at p50 and 2.8 ms at worst, and the other 21 about 0.5 ms together.
//! The device counters are read from the driver's memory. That is too slow for
//! the async worker and too often at the snapshot rate, so this sampler reads
//! on its own interval (`[samplers.network_efa] interval`, default 1s) in
//! `spawn_blocking`, as principle 17 requires, and serves the last values in
//! between.

const NAME: &str = "network_efa";

mod stats;

use stats::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::*;

use metriken::CounterGroup;

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SYSFS_INFINIBAND: &str = "/sys/class/infiniband";

const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(1);

/// How early a scrape may arrive and still be served with a fresh read, so
/// that a consumer scraping at `interval` does not have every other scrape
/// rejected by timer jitter.
const SCRAPE_TOLERANCE: Duration = Duration::from_millis(50);

/// What a port slot means across every port counter.
static PORT_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(PORT_IDENTITY_GROUPS);

/// What a device slot means across every device counter.
static DEVICE_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(DEVICE_IDENTITY_GROUPS);

fn init(config: Arc<Config>) -> SamplerResult {
    // Zero first, so every exit below leaves the groups empty rather than at
    // backing capacity, which would declare phantom all-null members.
    NETWORK_EFA_PORTS_ACQ.set_member_bound(0);
    NETWORK_EFA_DEVICES_ACQ.set_member_bound(0);

    if !config.enabled(NAME) {
        return Ok(None);
    }

    let efa = discover(Path::new(SYSFS_INFINIBAND));
    if efa.ports.is_empty() {
        return Err(crate::agent::sampler_status::Unsupported(
            "no EFA device with port counters found".to_string(),
        )
        .into());
    }

    // `discover` numbers ports and devices from 0, so prefix bounds are exact.
    NETWORK_EFA_PORTS_ACQ.set_member_bound(efa.ports.len());
    NETWORK_EFA_DEVICES_ACQ.set_member_bound(efa.devices.len());
    for port in &efa.ports {
        PORT_IDENTITY.assign(port.slot, port.labels.clone());
    }
    for device in &efa.devices {
        DEVICE_IDENTITY.assign(device.slot, device.labels.clone());
    }

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    debug!(
        "{NAME}: {} device(s), {} port(s), reading every {interval:?}",
        efa.devices.len(),
        efa.ports.len()
    );

    Ok(Some(Box::new(Efa {
        efa: Arc::new(std::sync::Mutex::new(efa)),
        interval,
        last_read: std::sync::Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Efa {
    /// Shared with the `spawn_blocking` task.
    efa: Arc<std::sync::Mutex<Discovered>>,
    /// Minimum time between reads (principle 17).
    interval: Duration,
    /// When the last read was dispatched.
    last_read: std::sync::Mutex<Option<Instant>>,
    /// Guards against overlapping reads if one runs long.
    reading: Arc<AtomicBool>,
}

#[async_trait]
impl Sampler for Efa {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        {
            let mut last = self.last_read.lock().unwrap();
            let due = self.interval.saturating_sub(SCRAPE_TOLERANCE);
            match *last {
                Some(t) if t.elapsed() < due => return,
                _ => *last = Some(Instant::now()),
            }
        }

        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        let efa = self.efa.clone();
        let reading = self.reading.clone();

        tokio::task::spawn_blocking(move || {
            if let Ok(mut efa) = efa.lock() {
                let efa = &mut *efa;
                sweep(&mut efa.devices, &NETWORK_EFA_DEVICES_ACQ);
                sweep(&mut efa.ports, &NETWORK_EFA_PORTS_ACQ);
            }
            reading.store(false, Ordering::Release);
        });
    }
}

/// Read every entity's counters inside one acquisition window.
fn sweep(entities: &mut [Entity], group: &AcquisitionGroup) {
    let guard = group.acquire();
    let mut read_any = false;
    for entity in entities.iter_mut() {
        read_any |= entity.refresh();
    }
    // With no counter read, no value is from inside this window, and stamping
    // it would present the old values as fresh.
    if read_any {
        guard.finish();
    } else {
        guard.discard();
    }
}

/// A port or a device: its slot in its counters' groups, its labels, and its
/// open counter files.
struct Entity {
    slot: usize,
    labels: BTreeMap<String, String>,
    counters: Vec<Counter>,
}

/// One open `hw_counters` file. Held open so a read is a single `pread`.
struct Counter {
    name: &'static str,
    file: File,
    metric: &'static CounterGroup,
    /// Whether a read has failed since the last good one, so a counter that
    /// keeps failing logs once.
    failing: bool,
}

impl Entity {
    /// Read and publish each counter. Returns whether any counter read; a
    /// counter that fails keeps its previous value.
    fn refresh(&mut self) -> bool {
        let mut buffer = [0; 32];
        let mut read_any = false;
        for counter in &mut self.counters {
            match read_counter(&counter.file, &mut buffer) {
                Ok(value) => {
                    counter.metric.set(self.slot, value);
                    counter.failing = false;
                    read_any = true;
                }
                Err(e) => {
                    if !counter.failing {
                        debug!("{NAME}: {:?} {}: {e}", self.labels, counter.name);
                    }
                    counter.failing = true;
                }
            }
        }
        read_any
    }
}

fn read_counter(file: &File, buffer: &mut [u8]) -> std::io::Result<u64> {
    let n = file.read_at(buffer, 0)?;
    parse_counter(&buffer[..n]).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("not a counter: {:?}", String::from_utf8_lossy(&buffer[..n])),
        )
    })
}

/// A `hw_counters` file's contents: a decimal integer and a newline.
fn parse_counter(contents: &[u8]) -> Option<u64> {
    std::str::from_utf8(contents).ok()?.trim().parse().ok()
}

/// The EFA ports and devices found, each numbered from 0.
struct Discovered {
    ports: Vec<Entity>,
    devices: Vec<Entity>,
}

/// The devices under `class` whose driver is `efa`, in name order, and their
/// ports in port-number order. A port or device with none of the known
/// counter files is left out.
fn discover(class: &Path) -> Discovered {
    let mut names: Vec<String> = std::fs::read_dir(class)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();

    let mut found = Discovered {
        ports: Vec::new(),
        devices: Vec::new(),
    };
    for name in names {
        let dir = class.join(&name);
        let driver = std::fs::read_link(dir.join("device/driver")).ok();
        if driver.as_deref().and_then(Path::file_name) != Some("efa".as_ref()) {
            continue;
        }

        let device_labels: BTreeMap<String, String> = [("device".to_string(), name.clone())].into();
        let counters = open_counters(&dir.join("hw_counters"), DEVICE_COUNTERS);
        if !counters.is_empty() {
            if found.devices.len() == MAX_DEVICES {
                warn!("{NAME}: more than {MAX_DEVICES} EFA devices; {name} is not reported");
            } else {
                found.devices.push(Entity {
                    slot: found.devices.len(),
                    labels: device_labels.clone(),
                    counters,
                });
            }
        }

        let mut ports: Vec<u32> = std::fs::read_dir(dir.join("ports"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect();
        ports.sort();
        for port in ports {
            let counters = open_counters(
                &dir.join("ports").join(port.to_string()).join("hw_counters"),
                PORT_COUNTERS,
            );
            if counters.is_empty() {
                continue;
            }
            if found.ports.len() == MAX_PORTS {
                warn!(
                    "{NAME}: more than {MAX_PORTS} EFA ports; {name} port {port} is not reported"
                );
                continue;
            }
            let mut labels = device_labels.clone();
            labels.insert("port".to_string(), port.to_string());
            found.ports.push(Entity {
                slot: found.ports.len(),
                labels,
                counters,
            });
        }
    }
    found
}

/// Open each of `known` that exists in `dir`.
fn open_counters(dir: &Path, known: &[(&'static str, &'static CounterGroup)]) -> Vec<Counter> {
    known
        .iter()
        .filter_map(|&(name, metric)| {
            let file = File::open(dir.join(name)).ok()?;
            Some(Counter {
                name,
                file,
                metric,
                failing: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture of one EFA device: each line is a path under the device
    /// directory and that file's contents.
    const CAPTURE: &str = include_str!("fixtures/rdmap0s27.txt");

    fn capture() -> Vec<(&'static str, &'static str)> {
        CAPTURE
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| line.split_once(' ').unwrap())
            .collect()
    }

    /// Lay out `device` under `class` from the capture, with its driver link
    /// pointing at a directory named `driver`.
    fn device(class: &Path, device: &str, driver: &str) {
        let dir = class.join(device);
        for (path, value) in capture() {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, format!("{value}\n")).unwrap();
        }
        let target = class.join("drivers").join(driver);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(dir.join("device")).unwrap();
        std::os::unix::fs::symlink(target, dir.join("device/driver")).unwrap();
    }

    fn names(entity: &Entity) -> Vec<&'static str> {
        entity.counters.iter().map(|c| c.name).collect()
    }

    #[test]
    fn the_captured_device_has_every_known_counter_and_nothing_else_is_read() {
        let class = tempfile::tempdir().unwrap();
        device(class.path(), "rdmap0s27", "efa");

        let found = discover(class.path());

        assert_eq!(found.ports.len(), 1);
        assert_eq!(found.devices.len(), 1);
        let port_files: Vec<&str> = PORT_COUNTERS.iter().map(|(n, _)| *n).collect();
        let device_files: Vec<&str> = DEVICE_COUNTERS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names(&found.ports[0]), port_files);
        assert_eq!(names(&found.devices[0]), device_files);
        assert_eq!(
            found.ports[0].labels,
            [("device", "rdmap0s27"), ("port", "1")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into()
        );
    }

    /// Every file of the capture is either a known counter or one this
    /// sampler leaves out on purpose, so a counter added to the capture
    /// without a metric fails here.
    #[test]
    fn every_captured_file_is_a_counter_or_left_out_on_purpose() {
        const LEFT_OUT: &[&str] = &[
            "lifespan",
            "submitted_cmds",
            "completed_cmds",
            "keep_alive_rcvd",
        ];
        for (path, _) in capture() {
            let file = path.rsplit('/').next().unwrap();
            let known = if path.starts_with("ports/") {
                PORT_COUNTERS.iter().any(|(n, _)| *n == file)
            } else {
                DEVICE_COUNTERS.iter().any(|(n, _)| *n == file)
            };
            assert!(known || LEFT_OUT.contains(&file), "{path}");
        }
    }

    #[test]
    fn other_rdma_drivers_are_not_read() {
        let class = tempfile::tempdir().unwrap();
        device(class.path(), "mlx5_0", "mlx5_core");

        let found = discover(class.path());

        assert!(found.ports.is_empty());
        assert!(found.devices.is_empty());
    }

    #[test]
    fn ports_are_numbered_across_devices_in_name_and_port_order() {
        let class = tempfile::tempdir().unwrap();
        device(class.path(), "rdmap1s0", "efa");
        device(class.path(), "rdmap0s27", "efa");
        let second = class.path().join("rdmap0s27/ports/2");
        std::fs::create_dir_all(second.join("hw_counters")).unwrap();
        std::fs::write(second.join("hw_counters/tx_bytes"), "5\n").unwrap();

        let found = discover(class.path());

        let ports: Vec<(usize, &str, &str)> = found
            .ports
            .iter()
            .map(|p| {
                (
                    p.slot,
                    p.labels["device"].as_str(),
                    p.labels["port"].as_str(),
                )
            })
            .collect();
        assert_eq!(
            ports,
            [
                (0, "rdmap0s27", "1"),
                (1, "rdmap0s27", "2"),
                (2, "rdmap1s0", "1")
            ]
        );
        assert_eq!(names(&found.ports[1]), ["tx_bytes"]);
        assert_eq!(found.devices.len(), 2);
    }

    #[test]
    fn a_host_with_no_infiniband_class_finds_nothing() {
        let found = discover(Path::new("/nonexistent/infiniband"));
        assert!(found.ports.is_empty());
        assert!(found.devices.is_empty());
    }

    #[test]
    fn a_counter_file_is_a_decimal_integer() {
        assert_eq!(parse_counter(b"19274743980032\n"), Some(19274743980032));
        assert_eq!(parse_counter(b"0\n"), Some(0));
        assert_eq!(parse_counter(b"18446744073709551615\n"), Some(u64::MAX));
        assert_eq!(parse_counter(b""), None);
        assert_eq!(parse_counter(b"-1\n"), None);
    }

    /// Uses the last slot, which no other test writes: the counters are
    /// process globals and tests run in parallel.
    #[test]
    fn a_refresh_publishes_the_captured_values() {
        let class = tempfile::tempdir().unwrap();
        device(class.path(), "rdmap0s27", "efa");
        let mut found = discover(class.path());
        found.ports[0].slot = MAX_PORTS - 1;
        found.devices[0].slot = MAX_DEVICES - 1;

        assert!(found.ports[0].refresh());
        assert!(found.devices[0].refresh());

        assert_eq!(
            NETWORK_EFA_RX_BYTES.value(MAX_PORTS - 1),
            Some(19274743980032)
        );
        assert_eq!(
            NETWORK_EFA_RDMA_READ_RESP_BYTES.value(MAX_PORTS - 1),
            Some(7408504012800)
        );
        assert_eq!(NETWORK_EFA_RETRANS_PKTS.value(MAX_PORTS - 1), Some(124166));
        assert_eq!(NETWORK_EFA_RX_DROPS.value(MAX_PORTS - 1), Some(204));
        assert_eq!(NETWORK_EFA_CMDS_ERR.value(MAX_DEVICES - 1), Some(0));
    }

    #[test]
    fn a_sweep_with_no_good_read_does_not_stamp_the_window() {
        let class = tempfile::tempdir().unwrap();
        device(class.path(), "rdmap0s27", "efa");
        let mut found = discover(class.path());
        found.ports.truncate(1);
        found.ports[0].counters.truncate(1);
        let tx = class.path().join("rdmap0s27/ports/1/hw_counters/tx_bytes");
        let group = AcquisitionGroup::new("test_sampler", "network_efa_failed_read");

        std::fs::write(&tx, "not a number\n").unwrap();
        sweep(&mut found.ports, &group);
        assert!(group.window().is_none());
        assert!(found.ports[0].counters[0].failing);

        std::fs::write(&tx, "7\n").unwrap();
        sweep(&mut found.ports, &group);
        assert!(group.window().is_some());
        assert!(!found.ports[0].counters[0].failing);
    }
}
