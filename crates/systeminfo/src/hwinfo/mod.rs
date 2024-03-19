use crate::Result;

mod cache;
mod cpu;
mod cpufreq;
mod interrupt;
mod memory;
mod net;
mod node;
mod sched_domain;
mod storage;
mod util;

pub use self::cache::{Cache, CacheType};
pub use self::cpu::Cpu;
pub use self::cpu::CpuSmt;
pub use self::cpufreq::CpuFreqBoosting;
pub use self::cpufreq::Cpufreq;
pub use self::interrupt::Interrupt;
pub use self::memory::Memory;
pub use self::net::{Interface, Queues};
pub use self::node::Node;
pub use self::sched_domain::SchedDomain;
pub use self::storage::Block;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HwInfo {
    pub kernel: String,
    pub caches: Vec<Vec<Cache>>,
    pub cpus: Vec<Cpu>,
    pub cpu_smt: CpuSmt,
    pub cpu_boosting: CpuFreqBoosting,
    pub memory: Memory,
    pub network: Vec<Interface>,
    pub blocks: Vec<Block>,
    pub nodes: Vec<Node>,
    pub interrupts: Vec<Interrupt>,
}

impl HwInfo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            kernel: self::util::read_string("/proc/version")?,
            caches: self::cache::get_caches()?,
            cpus: self::cpu::get_cpus()?,
            cpu_smt: self::cpu::get_cpu_smt(),
            cpu_boosting: self::cpufreq::get_cpu_boosting(),
            memory: Memory::new()?,
            network: self::net::get_interfaces(),
            blocks: self::storage::get_blocks(),
            nodes: self::node::get_nodes()?,
            interrupts: self::interrupt::get_interrupts(),
        })
    }

    pub fn get_cpus(&self) -> &Vec<Cpu> {
        &self.cpus
    }

    pub fn get_network_interfaces(&self) -> &Vec<Interface> {
        &self.network
    }
}
