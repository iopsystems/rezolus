use super::memory::Memory;
use super::util::*;
use crate::Result;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: usize,
    pub memory: Memory,
    pub cpus: Vec<usize>,
}

pub fn get_nodes() -> Result<Vec<Node>> {
    let mut ret = Vec::new();

    if let Ok(ids) = read_list("/sys/devices/system/node/online") {
        for id in ids {
            let memory = Memory::node(id)?;
            let cpus = read_list(format!("/sys/devices/system/node/node{id}/cpulist"))?;
            ret.push(Node { id, cpus, memory });
        }
    } else {
        // Some platforms might not expose node topology. For those platforms we
        // will consider all resources part of the same node.
        let memory = Memory::new()?;
        let cpus = read_list("/sys/devices/system/cpu/cpu0/topology/package_cpus_list")?;
        ret.push(Node {
            id: 0,
            cpus,
            memory,
        });
    }

    Ok(ret)
}
