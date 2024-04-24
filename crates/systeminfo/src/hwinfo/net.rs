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
    pub driver: Option<String>,
    pub irqs: Vec<usize>,
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queues {
    pub tx: Vec<TxQueue>,
    pub rx: Vec<RxQueue>,
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxQueue {
    pub id: u32,
    // whether xps is enabled or not
    pub xps: bool,
    // https://docs.kernel.org/networking/scaling.html
    pub xps_cpus: Vec<usize>,
    pub xps_rxqs: Vec<usize>,
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RxQueue {
    pub id: u32,
    // whether rps is enabled or not
    pub rps: bool,
    // https://docs.kernel.org/networking/scaling.html
    pub rps_cpus: Vec<usize>,
    pub rps_flow_cnt: usize,
}

fn get_interface(name: &OsStr) -> Result<Option<Interface>> {
    let name = name.to_str().ok_or_else(Error::invalid_interface_name)?;

    debug!("discovering network interface info for: {name}");

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
    let driver: Option<String> =
        match std::fs::read_link(format!("/sys/class/net/{name}/device/driver/module")) {
            Ok(driver_link) => Some(
                driver_link
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            ),
            Err(_) => None,
        };
    let irqs = read_irqs(format!("/sys/class/net/{name}/device/msi_irqs"));

    //  tx/rx queues
    let mut queues = Queues {
        tx: Vec::new(),
        rx: Vec::new(),
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
                if let Some(Ok(id)) = name.strip_prefix("tx-").map(|v| v.parse::<u32>()) {
                    let xps_cpus = read_hexbitmap(entry.path().join("xps_cpus"));
                    let xps_rxqs = read_hexbitmap(entry.path().join("xps_rxqs"));
                    let xps = !xps_cpus.is_empty() || !xps_rxqs.is_empty();
                    queues.tx.push(TxQueue {
                        id,
                        xps,
                        xps_cpus,
                        xps_rxqs,
                    });
                } else if let Some(Ok(id)) = name.strip_prefix("rx-").map(|v| v.parse::<u32>()) {
                    let rps_cpus = read_hexbitmap(entry.path().join("rps_cpus"));
                    let rps_flow_cnt = read_usize(entry.path().join("rps_flow_cnt")).unwrap();
                    let rps = !rps_cpus.is_empty();
                    queues.rx.push(RxQueue {
                        id,
                        rps,
                        rps_cpus,
                        rps_flow_cnt,
                    });
                }
            }
        }
    }

    debug!("completed discovery for network interface: {name}");

    Ok(Some(Interface {
        name: name.to_string(),
        carrier,
        mtu,
        node,
        speed,
        queues,
        driver,
        irqs,
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
