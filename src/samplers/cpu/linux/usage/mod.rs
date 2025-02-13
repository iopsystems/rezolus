/// Collects CPU usage stats using BPF and traces:
/// * `cpuacct_account_field`
///
/// And produces these stats:
/// * `cpu_usage`
/// * `cgroup_cpu_usage`

const NAME: &str = "cpu_usage";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::*;

use std::sync::Arc;

mod stats;

use stats::*;

unsafe impl plain::Plain for bpf::types::cgroup_info {}

fn handle_event(data: &[u8]) -> i32 {
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

        set_name(id as usize, name)
    }

    0
}

fn set_name(id: usize, name: String) {
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

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    set_name(1, "/".to_string());

    let counters = vec![
        &CPU_USAGE_USER,
        &CPU_USAGE_NICE,
        &CPU_USAGE_SYSTEM,
        &CPU_USAGE_SOFTIRQ,
        &CPU_USAGE_IRQ,
        &CPU_USAGE_STEAL,
        &CPU_USAGE_GUEST,
        &CPU_USAGE_GUEST_NICE,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("counters", counters)
        .packed_counters("cgroup_user", &CGROUP_CPU_USAGE_USER)
        .packed_counters("cgroup_nice", &CGROUP_CPU_USAGE_NICE)
        .packed_counters("cgroup_system", &CGROUP_CPU_USAGE_SYSTEM)
        .packed_counters("cgroup_softirq", &CGROUP_CPU_USAGE_SOFTIRQ)
        .packed_counters("cgroup_irq", &CGROUP_CPU_USAGE_IRQ)
        .packed_counters("cgroup_steal", &CGROUP_CPU_USAGE_STEAL)
        .packed_counters("cgroup_guest", &CGROUP_CPU_USAGE_GUEST)
        .packed_counters("cgroup_guest_nice", &CGROUP_CPU_USAGE_GUEST_NICE)
        .ringbuf_handler("cgroup_info", handle_event)
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
            "counters" => &self.maps.counters,
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
