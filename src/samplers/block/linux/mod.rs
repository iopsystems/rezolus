use super::stats::*;
use super::*;
use crate::common::{Counter, Nop};
use metriken::{DynBoxedMetric, MetricBuilder};
use samplers::hwinfo::hardware_info;
use std::fs::File;
use std::io::{Read, Seek};

const NAME: &str = "block_devices";

#[distributed_slice(BLOCK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(block) = BlockStat::new(config) {
        Box::new(block)
    } else {
        Box::new(Nop {})
    }
}

pub struct BlockStat {
    prev: Instant,
    next: Instant,
    interval: Duration,
    read_bytes: Counter,
    read_ios: Counter,
    write_bytes: Counter,
    write_ios: Counter,
    perblock_metrics: Vec<BlockMetrics>,
}

struct BlockMetrics {
    stat_file: File,
    read_bytes: DynBoxedMetric<metriken::Counter>,
    read_ios: DynBoxedMetric<metriken::Counter>,
    write_bytes: DynBoxedMetric<metriken::Counter>,
    write_ios: DynBoxedMetric<metriken::Counter>,
}

impl BlockStat {
    pub fn new(config: &Config) -> Result<Self, ()> {
        if !config.enabled(NAME) {
            return Err(());
        }

        let blocks = match hardware_info() {
            Ok(hwinfo) => hwinfo.get_storage_blocks(),
            Err(_) => return Err(()),
        };
        if blocks.is_empty() {
            return Err(());
        }

        let now = Instant::now();

        let mut perblock_metrics = Vec::with_capacity(blocks.len());

        for block in blocks {
            let name = &block.name;
            perblock_metrics.push(BlockMetrics {
                stat_file: File::open(format!("/sys/block/{name}/stat")).expect(&format!(
                    "Failed to open the stat file of the block device {name}"
                )),
                read_bytes: MetricBuilder::new("block/read/bytes")
                    .metadata("id", format!("{name}"))
                    .formatter(block_metric_formatter)
                    .build(metriken::Counter::new()),
                read_ios: MetricBuilder::new("block/read/ios")
                    .metadata("id", format!("{name}"))
                    .formatter(block_metric_formatter)
                    .build(metriken::Counter::new()),
                write_bytes: MetricBuilder::new("block/write/bytes")
                    .metadata("id", format!("{name}"))
                    .formatter(block_metric_formatter)
                    .build(metriken::Counter::new()),
                write_ios: MetricBuilder::new("block/write/ios")
                    .metadata("id", format!("{name}"))
                    .formatter(block_metric_formatter)
                    .build(metriken::Counter::new()),
            });
        }

        Ok(Self {
            prev: now,
            next: now,
            interval: config.interval(NAME),
            read_bytes: Counter::new(&BLOCK_READ_BYTES, Some(&BLOCK_READ_BYTES_HISTOGRAM)),
            read_ios: Counter::new(&BLOCK_READ_IOS, Some(&BLOCK_READ_IOS_HISTOGRAM)),
            write_bytes: Counter::new(&BLOCK_WRITE_BYTES, Some(&BLOCK_WRITE_BYTES_HISTOGRAM)),
            write_ios: Counter::new(&BLOCK_WRITE_IOS, Some(&BLOCK_WRITE_IOS_HISTOGRAM)),
            perblock_metrics,
        })
    }
}

impl Sampler for BlockStat {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if self.sample_blocks(elapsed).is_err() {
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

// The format of the block stat file explaned here https://docs.kernel.org/block/stat.html
// many variants are not used now, but we might need them later
#[allow(dead_code)]
enum BlockStatsItems {
    ReadIOs = 0,
    ReadMerges,
    ReadSectors,
    ReadTicks,
    WriteIOs,
    WriteMerges,
    WriteSectors,
    WriteTicks,
    InFlightRequests,
    IOTicks,
    TimeInQueue,
    DiscardIOs,
    DiscardMerges,
    DiscardSectors,
    DiscardTicks,
    FlushIOs,
    FlushTicks,
    Size = 17,
}

const BYTES_IN_SECTOR: u64 = 512;

impl BlockStat {
    fn sample_blocks(&mut self, elapsed: f64) -> Result<(), std::io::Error> {
        let mut total_read_bytes = 0;
        let mut total_read_ios = 0;
        let mut total_write_bytes = 0;
        let mut total_write_ios = 0;

        for block in &mut self.perblock_metrics {
            block.stat_file.rewind()?;
            let mut line = String::new();
            block.stat_file.read_to_string(&mut line)?;
            let parts: Vec<u64> = line
                .trim()
                .split_whitespace()
                .map(|v| v.parse::<u64>().unwrap())
                .collect();
            if parts.len() != BlockStatsItems::Size as usize {
                return Err(std::io::Error::other("wrong format in the block stat file"));
            }

            let read_bytes = parts[BlockStatsItems::ReadSectors as usize] * BYTES_IN_SECTOR;
            block.read_bytes.set(read_bytes);
            total_read_bytes += read_bytes;
            let read_ios = parts[BlockStatsItems::ReadIOs as usize];
            block.read_ios.set(read_ios);
            total_read_ios += read_ios;
            let write_bytes = parts[BlockStatsItems::WriteSectors as usize] * BYTES_IN_SECTOR;
            block.write_bytes.set(write_bytes);
            total_write_bytes += write_bytes;
            let write_ios = parts[BlockStatsItems::WriteIOs as usize];
            block.write_ios.set(write_ios);
            total_write_ios += write_ios;
        }
        self.read_bytes.set(elapsed, total_read_bytes);
        self.read_ios.set(elapsed, total_read_ios);
        self.write_bytes.set(elapsed, total_write_bytes);
        self.write_ios.set(elapsed, total_write_ios);
        Ok(())
    }
}
