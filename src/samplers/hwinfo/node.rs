use super::*;

#[derive(Serialize)]
pub struct Node {
    id: usize,
    memory: Memory,
    cpus: Vec<usize>,
}

pub fn get_nodes() -> Result<Vec<Node>> {
    let mut ret = Vec::new();

    let ids = read_list("/sys/devices/system/node/online")?;

    for id in ids {
        let memory = Memory::node(id)?;
        let cpus = read_list(format!("/sys/devices/system/node/node{id}/cpulist"))?;
        ret.push(Node { id, cpus, memory });
    }

    Ok(ret)
}
