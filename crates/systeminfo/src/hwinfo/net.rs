use std::ffi::OsStr;

use walkdir::WalkDir;

use super::util::*;
use crate::{Error, Result};

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interface {
    pub name: String,
    pub carrier: bool,
    pub speed: Option<usize>,
    pub node: Option<usize>,
    pub mtu: usize,
    pub queues: Queues,
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queues {
    pub tx: usize,
    pub rx: usize,
    pub combined: usize,
}

fn get_interface(name: &OsStr) -> Result<Option<Interface>> {
    let name = name.to_str().ok_or_else(Error::invalid_interface_name)?;

    // skip any that aren't "up"
    let operstate = read_string(format!("/sys/class/net/{name}/operstate"))?;
    if operstate != "up" {
        return Ok(None);
    }

    // get metadata we want
    let carrier = read_usize(format!("/sys/class/net/{name}/carrier")).map(|v| v == 1)?;
    let node = read_usize(format!("/sys/class/net/{name}/device/numa_node")).ok();
    let mtu = read_usize(format!("/sys/class/net/{name}/mtu"))?;
    let speed = read_usize(format!("/sys/class/net/{name}/speed")).ok();

    // count rx queues
    let mut queues = Queues {
        tx: 0,
        rx: 0,
        combined: 0,
    };

    let walker = WalkDir::new(format!("/sys/class/net/{name}/queues"))
        .follow_links(true)
        .max_depth(1)
        .into_iter();
    for entry in walker.filter_entry(|e| !is_hidden(e)) {
        if entry.is_err() {
            continue;
        }
        let entry = entry.unwrap();
        if entry.file_type().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("tx-") {
                    queues.tx += 1;
                } else if name.starts_with("rx-") {
                    queues.rx += 1;
                } else {
                    queues.combined += 1;
                }
            }
        }
    }

    Ok(Some(Interface {
        name: name.to_string(),
        carrier,
        mtu,
        node,
        speed,
        queues,
    }))
}

pub fn get_interfaces() -> Vec<Interface> {
    let mut ret = Vec::new();
    let walker = WalkDir::new("/sys/class/net/")
        .follow_links(true)
        .max_depth(1)
        .into_iter();
    for entry in walker.filter_entry(|e| !is_hidden(e)) {
        if entry.is_err() {
            continue;
        }
        let entry = entry.unwrap();
        if entry.file_type().is_dir() {
            if let Ok(Some(net)) = get_interface(entry.file_name()) {
                ret.push(net);
            }
        }
    }

    ret
}
