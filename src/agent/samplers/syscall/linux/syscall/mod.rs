//! Collects syscall counts and latencies using BPF and traces:
//! * `sys_enter` and `sys_exit` (raw tracepoints: `tp_btf`, or `raw_tp`
//!   without kernel BTF)
//!
//! And produces these stats:
//! * `syscall` (part `counts`)
//! * `cgroup_syscall` (part `counts`, with `cgroup_attribution`)
//! * `syscall_latency` (part `latency`)
//!
//! One program per hook: this sampler replaced `syscall_counts` and
//! `syscall_latency`, which each had a program on `sys_enter`
//! (docs/journal/2026-10-03-one-program-per-hook.md). The parts are the
//! config options `counts` and `latency`, both on by default; a config that
//! still names the old samplers is translated at load (`Config::load`).

const NAME: &str = "syscall";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/syscall_syscall.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use super::syscall_lut;
use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// The metrics a cgroup id reaches, one list per acquisition group.
///
/// One id spans several groups, and each group's schema carries its own copy
/// of the labels, so each group's metrics need their own entry.
static CGROUP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &CGROUP_SYSCALL_OTHER,
    &CGROUP_SYSCALL_READ,
    &CGROUP_SYSCALL_WRITE,
    &CGROUP_SYSCALL_POLL,
    &CGROUP_SYSCALL_LOCK,
    &CGROUP_SYSCALL_TIME,
    &CGROUP_SYSCALL_SLEEP,
    &CGROUP_SYSCALL_SOCKET,
    &CGROUP_SYSCALL_YIELD,
    &CGROUP_SYSCALL_FILESYSTEM,
    &CGROUP_SYSCALL_MEMORY,
    &CGROUP_SYSCALL_PROCESS,
    &CGROUP_SYSCALL_QUERY,
    &CGROUP_SYSCALL_IPC,
    &CGROUP_SYSCALL_TIMER,
    &CGROUP_SYSCALL_EVENT,
    &CGROUP_SYSCALL_SYNC,
]];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counts = config.part(NAME, "counts");
    let latency = config.part(NAME, "latency");
    if !counts && !latency {
        return Ok(None);
    }

    // A part that is off registers no metrics, so its groups have no
    // members; bound them, or every snapshot would carry them empty (the
    // sampler is live, so `bound_groups_without_a_live_sampler` skips them).
    if !counts {
        COUNTERS_ACQ.set_member_bound(0);
    }
    if !latency {
        LATENCIES_ACQ.set_member_bound(0);
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
        &SYSCALL_FILESYSTEM,
        &SYSCALL_MEMORY,
        &SYSCALL_PROCESS,
        &SYSCALL_QUERY,
        &SYSCALL_IPC,
        &SYSCALL_TIMER,
        &SYSCALL_EVENT,
        &SYSCALL_SYNC,
    ];

    let cgroup_attribution = counts && config.cgroup_attribution_or(NAME, true);

    // One of each tp_btf/raw_tp twin, and no exit program without latency.
    let mut disabled: Vec<&'static str> = if kernel_has_btf() {
        vec!["sys_enter_raw", "sys_exit_raw"]
    } else {
        vec!["sys_enter_btf", "sys_exit_btf"]
    };
    if !latency {
        disabled.extend(["sys_exit_btf", "sys_exit_raw"]);
        disabled.dedup();
    }

    let mut builder = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .map("syscall_lut", syscall_lut())
    .disabled_programs(&disabled)
    // The switches are read-only data the verifier folds at load (see
    // `counts`, `latency` and `cgroup_attribution` in mod.bpf.c); a part's
    // maps and series exist only when it is on.
    .pre_load(move |open| {
        let rodata = open
            .maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data");
        rodata.counts = counts as u8;
        rodata.latency = latency as u8;
        rodata.cgroup_attribution = cgroup_attribution as u8;
    });

    if counts {
        builder = builder.cpu_counters("counters", counters, &COUNTERS_ACQ);
    }

    if latency {
        // All 17 syscall-class latency histograms share ONE group: they are
        // LIKE ENTITIES (one "syscall latency" family, distinguished by the
        // `op` label) read as a single sweep — see stats.rs's `LATENCIES_ACQ`
        // doc comment. `BpfBuilder` batches every `.histogram()` call below
        // (same group reference) into one `HistogramBatch`, so it is stamped
        // once per refresh, not 17 times.
        builder = builder
            .histogram("other_latency", &SYSCALL_OTHER_LATENCY, &LATENCIES_ACQ)
            .histogram("read_latency", &SYSCALL_READ_LATENCY, &LATENCIES_ACQ)
            .histogram("write_latency", &SYSCALL_WRITE_LATENCY, &LATENCIES_ACQ)
            .histogram("poll_latency", &SYSCALL_POLL_LATENCY, &LATENCIES_ACQ)
            .histogram("lock_latency", &SYSCALL_LOCK_LATENCY, &LATENCIES_ACQ)
            .histogram("time_latency", &SYSCALL_TIME_LATENCY, &LATENCIES_ACQ)
            .histogram("sleep_latency", &SYSCALL_SLEEP_LATENCY, &LATENCIES_ACQ)
            .histogram("socket_latency", &SYSCALL_SOCKET_LATENCY, &LATENCIES_ACQ)
            .histogram("yield_latency", &SYSCALL_YIELD_LATENCY, &LATENCIES_ACQ)
            .histogram(
                "filesystem_latency",
                &SYSCALL_FILESYSTEM_LATENCY,
                &LATENCIES_ACQ,
            )
            .histogram("memory_latency", &SYSCALL_MEMORY_LATENCY, &LATENCIES_ACQ)
            .histogram("process_latency", &SYSCALL_PROCESS_LATENCY, &LATENCIES_ACQ)
            .histogram("query_latency", &SYSCALL_QUERY_LATENCY, &LATENCIES_ACQ)
            .histogram("ipc_latency", &SYSCALL_IPC_LATENCY, &LATENCIES_ACQ)
            .histogram("timer_latency", &SYSCALL_TIMER_LATENCY, &LATENCIES_ACQ)
            .histogram("event_latency", &SYSCALL_EVENT_LATENCY, &LATENCIES_ACQ)
            .histogram("sync_latency", &SYSCALL_SYNC_LATENCY, &LATENCIES_ACQ);
    }

    if cgroup_attribution {
        builder = builder
            .packed_counters(
                "cgroup_syscall_other",
                &CGROUP_SYSCALL_OTHER,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_read",
                &CGROUP_SYSCALL_READ,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_write",
                &CGROUP_SYSCALL_WRITE,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_poll",
                &CGROUP_SYSCALL_POLL,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_lock",
                &CGROUP_SYSCALL_LOCK,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_time",
                &CGROUP_SYSCALL_TIME,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_sleep",
                &CGROUP_SYSCALL_SLEEP,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_socket",
                &CGROUP_SYSCALL_SOCKET,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_yield",
                &CGROUP_SYSCALL_YIELD,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_filesystem",
                &CGROUP_SYSCALL_FILESYSTEM,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_memory",
                &CGROUP_SYSCALL_MEMORY,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_process",
                &CGROUP_SYSCALL_PROCESS,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_query",
                &CGROUP_SYSCALL_QUERY,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_ipc",
                &CGROUP_SYSCALL_IPC,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_timer",
                &CGROUP_SYSCALL_TIMER,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_event",
                &CGROUP_SYSCALL_EVENT,
                &CGROUP_COUNTERS_ACQ,
            )
            .packed_counters(
                "cgroup_syscall_sync",
                &CGROUP_SYSCALL_SYNC,
                &CGROUP_COUNTERS_ACQ,
            )
            .ringbuf_handler("cgroup_info", handle_cgroup_info);
    }

    let bpf = builder.build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_syscall_other" => &self.maps.cgroup_syscall_other,
            "cgroup_syscall_read" => &self.maps.cgroup_syscall_read,
            "cgroup_syscall_write" => &self.maps.cgroup_syscall_write,
            "cgroup_syscall_poll" => &self.maps.cgroup_syscall_poll,
            "cgroup_syscall_lock" => &self.maps.cgroup_syscall_lock,
            "cgroup_syscall_time" => &self.maps.cgroup_syscall_time,
            "cgroup_syscall_sleep" => &self.maps.cgroup_syscall_sleep,
            "cgroup_syscall_socket" => &self.maps.cgroup_syscall_socket,
            "cgroup_syscall_yield" => &self.maps.cgroup_syscall_yield,
            "cgroup_syscall_filesystem" => &self.maps.cgroup_syscall_filesystem,
            "cgroup_syscall_memory" => &self.maps.cgroup_syscall_memory,
            "cgroup_syscall_process" => &self.maps.cgroup_syscall_process,
            "cgroup_syscall_query" => &self.maps.cgroup_syscall_query,
            "cgroup_syscall_ipc" => &self.maps.cgroup_syscall_ipc,
            "cgroup_syscall_timer" => &self.maps.cgroup_syscall_timer,
            "cgroup_syscall_event" => &self.maps.cgroup_syscall_event,
            "cgroup_syscall_sync" => &self.maps.cgroup_syscall_sync,
            "counters" => &self.maps.counters,
            "syscall_lut" => &self.maps.syscall_lut,
            "other_latency" => &self.maps.other_latency,
            "read_latency" => &self.maps.read_latency,
            "write_latency" => &self.maps.write_latency,
            "poll_latency" => &self.maps.poll_latency,
            "lock_latency" => &self.maps.lock_latency,
            "time_latency" => &self.maps.time_latency,
            "sleep_latency" => &self.maps.sleep_latency,
            "socket_latency" => &self.maps.socket_latency,
            "yield_latency" => &self.maps.yield_latency,
            "filesystem_latency" => &self.maps.filesystem_latency,
            "memory_latency" => &self.maps.memory_latency,
            "process_latency" => &self.maps.process_latency,
            "query_latency" => &self.maps.query_latency,
            "ipc_latency" => &self.maps.ipc_latency,
            "timer_latency" => &self.maps.timer_latency,
            "event_latency" => &self.maps.event_latency,
            "sync_latency" => &self.maps.sync_latency,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} sys_enter_btf() BPF instruction count: {}",
            self.progs.sys_enter_btf.insn_cnt()
        );
        debug!(
            "{NAME} sys_enter_raw() BPF instruction count: {}",
            self.progs.sys_enter_raw.insn_cnt()
        );
        debug!(
            "{NAME} sys_exit_btf() BPF instruction count: {}",
            self.progs.sys_exit_btf.insn_cnt()
        );
        debug!(
            "{NAME} sys_exit_raw() BPF instruction count: {}",
            self.progs.sys_exit_raw.insn_cnt()
        );
    }
}
