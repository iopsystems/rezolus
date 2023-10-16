use crate::Result;

mod cache;
mod cpu;
mod memory;
mod net;
mod node;
mod util;

pub use self::cache::{Cache, CacheType};
pub use self::cpu::Cpu;
pub use self::memory::Memory;
pub use self::net::{Interface, Queues};
pub use self::node::Node;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HwInfo {
    pub caches: Vec<Vec<Cache>>,
    pub cpus: Vec<Cpu>,
    pub memory: Memory,
    pub network: Vec<Interface>,
    pub nodes: Vec<Node>,
}

impl HwInfo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            caches: self::cache::get_caches()?,
            cpus: self::cpu::get_cpus()?,
            memory: Memory::new()?,
            network: self::net::get_interfaces(),
            nodes: self::node::get_nodes()?,
        })
    }

    pub fn get_cpus(&self) -> &Vec<Cpu> {
        &self.cpus
    }
}
