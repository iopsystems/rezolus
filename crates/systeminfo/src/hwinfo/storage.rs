use std::ffi::OsStr;

use walkdir::WalkDir;

use super::util::*;
use crate::{Error, Result};

const LOGIC_SECTOR_SIZE: usize = 512;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
// fs -> block -> device
pub struct Block {
    // Blocks under /sys/block except loop devices
    pub name: String,
    // Device size in bytes /sys/block/NAME/size * 512
    pub size: usize,
    // pub model: Option<String>,
    // pub numa_node: Option<usize>,
    // // mq
    // pub ioqeues: Queues,
    // // hardware queue
    // pub device_queue_size: usize,
    // pub irqs: Vec<usize>,
    // pub speed: Option<usize>,
    // pub node: Option<usize>,
    // pub mtu: usize,
    // pub queues: Queues,
}

// // https://www.kernel.org/doc/Documentation/block/queue-sysfs.txt
// pub struct BlockQueue {
//     // number of requests in the block layer for read or write requests
//     pub nr_requests: usize,
//     // whether polling is enabled or not
//     pub io_poll: bool,
//     pub logical_block_size: usize,
//     // Smallest unit in bytes wihtout read-modify-write
//     pub physical_block_size: usize,
//     // Hard maximum logic sectors (512 bytes) a device can handler per request
//     pub max_hw_sectors: usize,
//     // Soft maximum logic sectors used by VFS for buffered IO
//     pub max_sectors: usize,
//     // Device preferred request size in bytes
//     pub optimal_io_size: usize,
//     // Maximum number of segment
//     pub max_segment_size: usize,
//     pub max_segments: usize,
// }

fn get_block(name: &OsStr) -> Result<Option<Block>> {
    let name = name.to_str().ok_or_else(Error::invalid_block_name)?;
    if name.starts_with("loop") {
        return Ok(None);
    }
    let size = read_usize(format!("/sys/block/{name}/size"))? * LOGIC_SECTOR_SIZE;
    Ok(Some(Block {
        name: name.to_string(),
        size,
    }))
}

pub fn get_blocks() -> Vec<Block> {
    let mut ret = Vec::new();
    let walker = WalkDir::new("/sys/block")
        .follow_links(true)
        .max_depth(1)
        .into_iter();
    for entry in walker.filter_entry(|e| !is_hidden(e)) {
        if entry.is_err() {
            continue;
        }
        let entry = entry.unwrap();
        if entry.file_type().is_dir() {
            if let Ok(Some(block)) = get_block(entry.file_name()) {
                ret.push(block);
            }
        }
    }

    ret
}
