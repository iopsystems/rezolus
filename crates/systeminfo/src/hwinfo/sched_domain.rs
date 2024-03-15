use super::util::*;

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
    pub fn new(cpu: usize, domain: &str) -> Self {
        let name = read_string(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/name"
        ))
        .unwrap();
        let flags = read_space_list(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/flags"
        ))
        .unwrap();
        let min_interval = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/min_interval"
        ))
        .unwrap();
        let max_interval = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/max_interval"
        ))
        .unwrap();
        let imbalance_pct = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/imbalance_pct"
        ))
        .unwrap();
        let cache_nice_tries = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/cache_nice_tries"
        ))
        .unwrap();
        let busy_factor = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/busy_factor"
        ))
        .unwrap();
        let max_newidle_lb_cost = read_usize(format!(
            "/sys/kernel/debug/sched/domains/cpu{cpu}/{domain}/max_newidle_lb_cost"
        ))
        .unwrap();
        SchedDomain {
            name,
            flags,
            min_interval,
            max_interval,
            imbalance_pct,
            cache_nice_tries,
            busy_factor,
            max_newidle_lb_cost,
        }
    }
}
