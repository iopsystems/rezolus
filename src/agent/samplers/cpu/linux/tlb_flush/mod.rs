//! Collects tlb flush event information using BPF and traces:
//! * `tlb_flush`
//!
//! And produces these stats:
//! * `cpu_tlb_flush`
//! * `cgroup_cpu_tlb_flush`
//!
//! These stats can be used to understand the reason for TLB flushes.

const NAME: &str = "cpu_tlb_flush";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_tlb_flush.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

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
        CGROUP_TLB_FLUSH_TASK_SWITCH.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN.insert_metadata(id, "name".to_string(), name.clone());
        CGROUP_TLB_FLUSH_REMOTE_SEND_IPI.insert_metadata(id, "name".to_string(), name);
    }
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let events = vec![
        &TLB_FLUSH_TASK_SWITCH,
        &TLB_FLUSH_REMOTE_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
        &TLB_FLUSH_REMOTE_SEND_IPI,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("events", events)
        .packed_counters("cgroup_task_switch", &CGROUP_TLB_FLUSH_TASK_SWITCH)
        .packed_counters(
            "cgroup_remote_shootdown",
            &CGROUP_TLB_FLUSH_REMOTE_SHOOTDOWN,
        )
        .packed_counters("cgroup_local_shootdown", &CGROUP_TLB_FLUSH_LOCAL_SHOOTDOWN)
        .packed_counters(
            "cgroup_local_mm_shootdown",
            &CGROUP_TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
        )
        .packed_counters("cgroup_remote_send_ipi", &CGROUP_TLB_FLUSH_REMOTE_SEND_IPI)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_task_switch" => &self.maps.cgroup_task_switch,
            "cgroup_remote_shootdown" => &self.maps.cgroup_remote_shootdown,
            "cgroup_local_shootdown" => &self.maps.cgroup_local_shootdown,
            "cgroup_local_mm_shootdown" => &self.maps.cgroup_local_mm_shootdown,
            "cgroup_remote_send_ipi" => &self.maps.cgroup_remote_send_ipi,
            "events" => &self.maps.events,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tlb_flush() BPF instruction count: {}",
            self.progs.tlb_flush.insn_cnt()
        );
    }
}
