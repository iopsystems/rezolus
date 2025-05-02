//! Collects CPU usage stats using BPF and traces:
//! * `cpuacct_account_field`
//! * `softirq_entry`
//! * `softirq_exit`
//!
//! And produces these stats:
//! * `cpu_usage`
//! * `cgroup_cpu_usage`
//! * `softirq`
//! * `softirq_time`
//!
//! Note: softirq is included because we need to trace softirq entry/exit in
//! order to provide accurate accounting of cpu_usage for softirq. That makes
//! these additional metrics free.

const NAME: &str = "cpu_usage";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
unsafe impl plain::Plain for bpf::types::task_info {}

fn handle_cgroup_info(data: &[u8]) -> i32 {
    let mut cgroup_info = bpf::types::cgroup_info::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(&cgroup_info.name)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let pname = std::str::from_utf8(&cgroup_info.pname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let gpname = std::str::from_utf8(&cgroup_info.gpname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let name = if !gpname.is_empty() {
            if cgroup_info.level > 3 {
                format!(".../{gpname}/{pname}/{name}")
            } else {
                format!("/{gpname}/{pname}/{name}")
            }
        } else if !pname.is_empty() {
            format!("/{pname}/{name}")
        } else if !name.is_empty() {
            format!("/{name}")
        } else {
            "".to_string()
        };

        let id = cgroup_info.id;

        set_cgroup_name(id as usize, name)
    }

    0
}

fn set_cgroup_name(id: usize, name: String) {
    if !name.is_empty() {
        CGROUP_CPU_USAGE_USER.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_NICE.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_SYSTEM.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_SOFTIRQ.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_IRQ.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_STEAL.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_GUEST.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_CPU_USAGE_GUEST_NICE.insert_metadata(id, "name".to_string(), name);
    }
}

fn handle_pid_info(data: &[u8]) -> i32 {
    let mut info = bpf::types::pid_info::default();

    if plain::copy_from_bytes(&mut info, data).is_ok() {
        let name = std::str::from_utf8(&info.name)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cg_name = std::str::from_utf8(&info.cg_name)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cg_pname = std::str::from_utf8(&info.cg_pname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cg_gpname = std::str::from_utf8(&info.cg_gpname)
            .unwrap()
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cg_name = if !cg_gpname.is_empty() {
            if info.cglevel > 3 {
                format!(".../{cg_gpname}/{cg_pname}/{cg_name}")
            } else {
                format!("/{cg_gpname}/{cg_pname}/{cg_name}")
            }
        } else if !cg_pname.is_empty() {
            format!("/{cg_pname}/{cg_name}")
        } else if !cg_name.is_empty() {
            format!("/{cg_name}")
        } else {
            "".to_string()
        };

        set_pid_name(info.pid as usize, info.tgid as usize, name, cg_name);
    }

    0
}

fn set_task_name(id: usize, tgid: usize, name: String, cgroup: String) {
    if !name.is_empty() {
        TASK_CPU_USAGE.insert_metadata(id, "tgid".to_string(), format!("{tgid}"));
        TASK_CPU_USAGE.insert_metadata(id, "name".to_string(), name.clone());

        if !cgroup.is_empty() {
            TASK_CPU_USAGE.insert_metadata(id, "cgroup".to_string(), cgroup.clone());
        }
    }
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    set_cgroup_name(1, "/".to_string());

    let cpu_usage = vec![
        &CPU_USAGE_USER,
        &CPU_USAGE_NICE,
        &CPU_USAGE_SYSTEM,
        &CPU_USAGE_SOFTIRQ,
        &CPU_USAGE_IRQ,
        &CPU_USAGE_STEAL,
        &CPU_USAGE_GUEST,
        &CPU_USAGE_GUEST_NICE,
    ];

    let softirq = vec![
        &SOFTIRQ_HI,
        &SOFTIRQ_TIMER,
        &SOFTIRQ_NET_TX,
        &SOFTIRQ_NET_RX,
        &SOFTIRQ_BLOCK,
        &SOFTIRQ_IRQ_POLL,
        &SOFTIRQ_TASKLET,
        &SOFTIRQ_SCHED,
        &SOFTIRQ_HRTIMER,
        &SOFTIRQ_RCU,
    ];

    let softirq_time = vec![
        &SOFTIRQ_TIME_HI,
        &SOFTIRQ_TIME_TIMER,
        &SOFTIRQ_TIME_NET_TX,
        &SOFTIRQ_TIME_NET_RX,
        &SOFTIRQ_TIME_BLOCK,
        &SOFTIRQ_TIME_IRQ_POLL,
        &SOFTIRQ_TIME_TASKLET,
        &SOFTIRQ_TIME_SCHED,
        &SOFTIRQ_TIME_HRTIMER,
        &SOFTIRQ_TIME_RCU,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("cpu_usage", cpu_usage)
        .cpu_counters("softirq", softirq)
        .cpu_counters("softirq_time", softirq_time)
        .packed_counters("cgroup_user", &CGROUP_CPU_USAGE_USER)
        .packed_counters("cgroup_nice", &CGROUP_CPU_USAGE_NICE)
        .packed_counters("cgroup_system", &CGROUP_CPU_USAGE_SYSTEM)
        .packed_counters("cgroup_softirq", &CGROUP_CPU_USAGE_SOFTIRQ)
        .packed_counters("cgroup_irq", &CGROUP_CPU_USAGE_IRQ)
        .packed_counters("cgroup_steal", &CGROUP_CPU_USAGE_STEAL)
        .packed_counters("cgroup_guest", &CGROUP_CPU_USAGE_GUEST)
        .packed_counters("cgroup_guest_nice", &CGROUP_CPU_USAGE_GUEST_NICE)
        .packed_counters("task_usage", &TASK_CPU_USAGE)
        .ringbuf_handler("cgroup_info", handle_cgroup_info)
        .ringbuf_handler("task_info", handle_task_info)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_user" => &self.maps.cgroup_user,
            "cgroup_nice" => &self.maps.cgroup_nice,
            "cgroup_system" => &self.maps.cgroup_system,
            "cgroup_softirq" => &self.maps.cgroup_softirq,
            "cgroup_irq" => &self.maps.cgroup_irq,
            "cgroup_steal" => &self.maps.cgroup_steal,
            "cgroup_guest" => &self.maps.cgroup_guest,
            "cgroup_guest_nice" => &self.maps.cgroup_guest_nice,
            "cpu_usage" => &self.maps.cpu_usage,
            "task_info" => &self.maps.task_info,
            "task_usage" => &self.maps.task_usage,
            "softirq" => &self.maps.softirq,
            "softirq_time" => &self.maps.softirq_time,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} cpuacct_account_field() BPF instruction count: {}",
            self.progs.cpuacct_account_field_kprobe.insn_cnt()
        );
    }
}
