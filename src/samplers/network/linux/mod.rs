use super::stats::*;
use super::*;
use crate::common::{Counter, Nop};
use metriken::{DynBoxedMetric, MetricBuilder};
use samplers::hwinfo::hardware_info;
use std::fs::File;
use std::io::{Error, Read, Seek};

const NAME: &str = "network_interface";

#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(networkstat) = NetworkInterfaceStat::new(config) {
        Box::new(networkstat)
    } else {
        Box::new(Nop {})
    }
}

pub struct NetworkInterfaceStat {
    prev: Instant,
    next: Instant,
    interval: Duration,
    rx_bytes: Counter,
    rx_packets: Counter,
    tx_bytes: Counter,
    tx_packets: Counter,
    pernic_metrics: Vec<NetworkMetrics>,
}

struct NetworkMetrics {
    // NIC name
    name: String,
    // /sys/class/net/[name]/statistics/rx_bytes
    rx_bytes_file: File,
    rx_bytes: DynBoxedMetric<metriken::Counter>,
    // /sys/class/net/[name]/statistics/rx_pockets
    rx_packets_file: File,
    rx_packets: DynBoxedMetric<metriken::Counter>,
    // /sys/class/net/[name]/statistics/tx_bytes
    tx_bytes_file: File,
    tx_bytes: DynBoxedMetric<metriken::Counter>,
    // /sys/class/net/[name]/statistics/rx_pockets
    tx_packets_file: File,
    tx_packets: DynBoxedMetric<metriken::Counter>,
}

impl NetworkInterfaceStat {
    pub fn new(config: &Config) -> Result<Self, ()> {
        if !config.enabled(NAME) {
            return Err(());
        }

        let nics = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_network_interfaces(),
            Err(_) => return Err(()),
        };

        // No active NICs
        if nics.is_empty() {
            return Err(());
        }

        let now = Instant::now();

        let mut pernic_metrics = Vec::with_capacity(nics.len());

        for interface in nics {
            let name = &interface.name;
            pernic_metrics.push(NetworkMetrics {
                name: interface.name.clone(),
                rx_bytes_file: File::open(format!("/sys/class/net/{name}/statistics/rx_bytes"))
                    .expect("file not found"),
                rx_bytes: MetricBuilder::new("network/receive/bytes")
                    .metadata("id", format!("{name}"))
                    .formatter(network_metric_formatter)
                    .build(metriken::Counter::new()),

                rx_packets_file: File::open(format!("/sys/class/net/{name}/statistics/rx_packets"))
                    .expect("file not found"),
                rx_packets: MetricBuilder::new("network/receive/packets")
                    .metadata("id", format!("{name}"))
                    .formatter(network_metric_formatter)
                    .build(metriken::Counter::new()),

                tx_bytes_file: File::open(format!("/sys/class/net/{name}/statistics/tx_bytes"))
                    .expect("file not found"),
                tx_bytes: MetricBuilder::new("network/transmit/bytes")
                    .metadata("id", format!("{name}"))
                    .formatter(network_metric_formatter)
                    .build(metriken::Counter::new()),

                tx_packets_file: File::open(format!("/sys/class/net/{name}/statistics/tx_packets"))
                    .expect("file not found"),
                tx_packets: MetricBuilder::new("network/transmit/packets")
                    .metadata("id", format!("{name}"))
                    .formatter(network_metric_formatter)
                    .build(metriken::Counter::new()),
            });
        }

        Ok(Self {
            prev: now,
            next: now,
            interval: config.interval(NAME),
            rx_bytes: Counter::new(&NETWORK_RX_BYTES, Some(&NETWORK_RX_BYTES_HISTOGRAM)),
            rx_packets: Counter::new(&NETWORK_RX_PACKETS, Some(&NETWORK_RX_PACKETS_HISTOGRAM)),
            tx_bytes: Counter::new(&NETWORK_TX_BYTES, Some(&NETWORK_TX_BYTES_HISTOGRAM)),
            tx_packets: Counter::new(&NETWORK_TX_PACKETS, Some(&NETWORK_TX_PACKETS_HISTOGRAM)),
            pernic_metrics,
        })
    }
}

impl Sampler for NetworkInterfaceStat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if self.sample_network_interfaces(elapsed).is_err() {
            return;
        }

        // determine when to sample next
        let next = self.next + self.interval;

        // it's possible we fell behind
        if next > now {
            // if we didn't, sample at the next planned time
            self.next = next;
        } else {
            // if we did, sample after the interval has elapsed
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}

fn read_u64(file: &mut File) -> Result<u64, std::io::Error> {
    file.rewind()?;
    let mut data = String::new();
    file.read_to_string(&mut data);
    data.trim()
        .parse::<u64>()
        .map_err(|e| std::io::Error::other(e.to_string()))
    // data.parse::<u64>().map_err(|e| std::io::Error::new(ErrorKind::e.to_string()))
}

impl NetworkInterfaceStat {
    fn sample_network_interfaces(&mut self, elapsed: f64) -> Result<(), std::io::Error> {
        let mut total_rx_bytes = 0;
        let mut total_rx_packets = 0;
        let mut total_tx_bytes = 0;
        let mut total_tx_packets = 0;
        for nic in &mut self.pernic_metrics {
            let rx_bytes = read_u64(&mut nic.rx_bytes_file)?;
            let rx_packets = read_u64(&mut nic.rx_packets_file)?;
            let tx_bytes = read_u64(&mut nic.tx_bytes_file)?;
            let tx_packets = read_u64(&mut nic.tx_packets_file)?;

            total_rx_bytes += rx_bytes;
            nic.rx_bytes.set(rx_bytes);
            total_rx_packets += rx_packets;
            nic.rx_packets.set(rx_packets);
            total_tx_bytes += tx_bytes;
            nic.tx_bytes.set(tx_bytes);
            total_tx_packets += tx_packets;
            nic.tx_packets.set(tx_packets);
        }
        self.rx_bytes.set(elapsed, total_rx_bytes);
        self.rx_packets.set(elapsed, total_rx_packets);
        self.tx_bytes.set(elapsed, total_tx_bytes);
        self.tx_packets.set(elapsed, total_tx_packets);
        Ok(())
    }
}
