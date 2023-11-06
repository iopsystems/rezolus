use std::ffi::OsStr;

use super::util::*;
use crate::Result;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
//https://www.kernel.org/doc/Documentation/scheduler/sched-domains.txt
pub struct SchedDomain {
    pub name: String,
    pub flags: Vec<String>,
    pub min_interval: usize,
    pub max_interval: usize,
    pub imbalance_pct: usize,
    pub cache_nice_tries: usize,
    pub busy_factor: usize,
    pub max_newidle_lb_cost: usize,
}

impl SchedDomain {
    pub fn new(cpu: usize, domain: &str) -> Result<Self> {
        let name = read_string(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/name"
        ))?;
        let flags = read_space_list(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/flags"
        ))?;
        let min_interval = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/min_interval"
        ))?;
        let max_interval = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/max_interval"
        ))?;
        let imbalance_pct = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/imbalance_pct"
        ))?;
        let cache_nice_tries = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/cache_nice_tries"
        ))?;
        let busy_factor = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/busy_factor"
        ))?;
        let max_newidle_lb_cost = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/max_newidle_lb_cost"
        ))?;
        Ok(SchedDomain {
            name,
            flags,
            min_interval,
            max_interval,
            imbalance_pct,
            cache_nice_tries,
            busy_factor,
            max_newidle_lb_cost,
        })
    }
}
