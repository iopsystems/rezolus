/// Collects Syscall stats using BPF and traces:
/// * `raw_syscalls/sys_enter`
///
/// And produces these stats:
/// * `syscall/total`
/// * `syscall/read`
/// * `syscall/write`
/// * `syscall/poll`
/// * `syscall/lock`
/// * `syscall/time`
/// * `syscall/sleep`
/// * `syscall/socket`
/// * `syscall/yield`

const NAME: &str = "syscall_counts";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_counts.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::common::*;
use crate::samplers::syscall::linux::syscall_lut;
use crate::*;

use std::sync::Arc;

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
            format!("{gpname}_{pname}_{name}")
        } else if !pname.is_empty() {
            format!("{pname}_{name}")
        } else {
            name.to_string()
        };

        let id = cgroup_info.id;

        if !name.is_empty() {
            CGROUP_SYSCALL_OTHER.insert_metadata(id as usize, "name".to_string(), name.clone());
            CGROUP_SYSCALL_READ.insert_metadata(id as usize, "name".to_string(), name.clone());
            CGROUP_SYSCALL_WRITE.insert_metadata(id as usize, "name".to_string(), name);
        }
    }

    0
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &SYSCALL_OTHER,
        &SYSCALL_READ,
        &SYSCALL_WRITE,
        &SYSCALL_POLL,
        &SYSCALL_LOCK,
        &SYSCALL_TIME,
        &SYSCALL_SLEEP,
        &SYSCALL_SOCKET,
        &SYSCALL_YIELD,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .map("syscall_lut", syscall_lut())
        .packed_counters("cgroup_syscall_other", &CGROUP_SYSCALL_OTHER)
        .packed_counters("cgroup_syscall_read", &CGROUP_SYSCALL_READ)
        .packed_counters("cgroup_syscall_write", &CGROUP_SYSCALL_WRITE)
        .ringbuf_handler("cgroup_info", handle_event)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_syscall_other" => &self.maps.cgroup_syscall_other,
            "cgroup_syscall_read" => &self.maps.cgroup_syscall_read,
            "cgroup_syscall_write" => &self.maps.cgroup_syscall_write,
            "counters" => &self.maps.counters,
            "syscall_lut" => &self.maps.syscall_lut,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} sys_enter() BPF instruction count: {}",
            self.progs.sys_enter.insn_cnt()
        );
    }
}
