use std::ffi::OsStr;

use walkdir::WalkDir;

use super::util::*;
use crate::{Error, Result};

const LOGIC_SECTOR_SIZE: usize = 512;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
// Block devices under /sys/block except virtual loop devices
// TODO: add hardware queue, block queue, and IRQ information
pub struct Block {
    pub name: String,
    // Device size in bytes /sys/block/NAME/size * 512
    pub size: usize,
}

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
