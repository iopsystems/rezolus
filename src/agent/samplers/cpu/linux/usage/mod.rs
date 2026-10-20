//! Collects CPU usage stats using BPF and traces:
//! * `cpuacct_account_field`
//! * `softirq_entry`
//! * `softirq_exit`
//! * `sched_process_exit`
//! * `sched_process_exec`
//! * `task_rename` — only with `task_attribution = true`
//!
//! And produces these stats:
//! * `cpu_usage`
//! * `cgroup_cpu_usage`
//! * `task_cpu_usage` — only with `task_attribution = true` (off by default)
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
impl_cgroup_info!(bpf::types::cgroup_info);

unsafe impl plain::Plain for bpf::types::task_info {}
unsafe impl plain::Plain for bpf::types::task_exit {}

/// The metrics a cgroup id reaches, one list per acquisition group.
///
/// One id spans several groups, and each group's schema carries its own copy
/// of the labels, so each group's metrics need their own entry.
static CGROUP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[
    &[&CGROUP_CPU_USAGE_EXITED],
    &[&CGROUP_CPU_USAGE_USER, &CGROUP_CPU_USAGE_SYSTEM],
];
static TASK_METRICS: &[&dyn metriken::group::SlotMetadata] = &[&TASK_CPU_USAGE];

/// The only way this sampler writes what a task slot means. The group and the
/// metrics are bound here so no call site can pair them wrongly.
static TASK_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(TASK_IDENTITY_GROUPS);

static TASK_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[TASK_METRICS];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

/// The newest `ts` of the `task_info` events applied to each labelled task
/// slot.
///
/// `task_info` and `task_exit` arrive on separate ringbufs, and one drain
/// applies every `task_info` before any `task_exit`. When a pid's old
/// occupant exits and its new occupant's `task_info` is sent in the same
/// snapshot interval (nearly always so for a thread that execs from outside
/// the group leader), the old occupant's exit is applied after the new
/// labels.
/// An exit older than the slot's last assignment must not release it.
///
/// A `task_info` for the exiting task itself stamped after its exit (a
/// rename issued from another CPU as the task exits) also holds the slot:
/// the exited task's labels stay, reading 0, until the pid is reused, as
/// after a dropped `task_exit`.
static ASSIGNED_AT: std::sync::Mutex<std::collections::BTreeMap<usize, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn handle_task_info(data: &[u8]) -> i32 {
    let mut info = bpf::types::task_info::default();

    if plain::copy_from_bytes(&mut info, data).is_ok() {
        let pid = info.pid as usize;
        let slot = pid_slot(pid);
        let tgid = info.tgid;

        // Extract comm (task name)
        let comm = std::str::from_utf8(&info.comm)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .to_string();

        let cgroup_name = std::str::from_utf8(&info.cgroup_name)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cgroup_pname = std::str::from_utf8(&info.cgroup_pname)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cgroup_gpname = std::str::from_utf8(&info.cgroup_gpname)
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let cgroup = if !cgroup_gpname.is_empty() {
            if info.cgroup_level > 3 {
                format!(".../{cgroup_gpname}/{cgroup_pname}/{cgroup_name}")
            } else {
                format!("/{cgroup_gpname}/{cgroup_pname}/{cgroup_name}")
            }
        } else if !cgroup_pname.is_empty() {
            format!("/{cgroup_pname}/{cgroup_name}")
        } else if !cgroup_name.is_empty() {
            format!("/{cgroup_name}")
        } else {
            "/".to_string()
        };

        // Built once and set once. Setting the four labels separately left a
        // window where a reader could see a slot half-way through changing
        // hands — the new task's pid beside the old task's comm. See
        // `agent::identity`.
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("pid".to_string(), pid.to_string());
        labels.insert("tgid".to_string(), tgid.to_string());
        if !comm.is_empty() {
            labels.insert("comm".to_string(), comm);
        }
        labels.insert("cgroup".to_string(), cgroup);

        let mut assigned = ASSIGNED_AT.lock().unwrap_or_else(|e| e.into_inner());
        TASK_IDENTITY.assign(slot, labels);
        let at = assigned.entry(slot).or_insert(info.ts);
        *at = (*at).max(info.ts);
    }

    0
}

fn handle_task_exit(data: &[u8]) -> i32 {
    let mut exit = bpf::types::task_exit::default();

    if plain::copy_from_bytes(&mut exit, data).is_ok() {
        let slot = pid_slot(exit.pid as usize);

        let mut assigned = ASSIGNED_AT.lock().unwrap_or_else(|e| e.into_inner());
        if assigned.get(&slot).is_some_and(|&at| at > exit.ts) {
            // A task_info sent after this exit holds the slot; normally
            // the pid's next occupant's (see ASSIGNED_AT).
            return 0;
        }
        assigned.remove(&slot);
        TASK_IDENTITY.release(slot);
    }

    0
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // order must match the offsets in mod.bpf.c
    let cpu_usage = vec![&CPU_USAGE_USER, &CPU_USAGE_SYSTEM, &CPU_USAGE_EXITED];

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

    let task_attribution = config.task_attribution(NAME);
    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);

    let fentry = kernel_can_fentry("cpuacct_account_field");
    let btf = kernel_has_btf();

    // BTF present: the tp_btf twin of each tracepoint program, else its
    // raw_tp twin. cpuacct_account_field: the fentry twin (cheaper dispatch)
    // where fentry can attach to it (`kernel_can_fentry`), else the kprobe.
    // task_rename relabels per-task series, so neither twin loads without
    // task_attribution.
    let mut disabled = vec![
        if btf {
            "handle__sched_process_exit_raw"
        } else {
            "handle__sched_process_exit_btf"
        },
        if btf {
            "handle__sched_process_exec_raw"
        } else {
            "handle__sched_process_exec_btf"
        },
        if fentry {
            "cpuacct_account_field_kprobe"
        } else {
            "cpuacct_account_field_fentry"
        },
    ];
    if !task_attribution {
        disabled.extend(["handle__task_rename_btf", "handle__task_rename_raw"]);
    } else if btf {
        disabled.push("handle__task_rename_raw");
    } else {
        disabled.push("handle__task_rename_btf");
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
    .cpu_counters("cpu_usage", cpu_usage, &CPU_USAGE_ACQ)
    .cpu_counters("softirq", softirq, &SOFTIRQ_ACQ)
    .cpu_counters("softirq_time", softirq_time, &SOFTIRQ_TIME_ACQ)
    .disabled_programs(&disabled)
    // Losing this probe (e.g. CONFIG_TICK_CPU_ACCOUNTING kernels) drops the
    // entire CPU-time-by-category breakdown; label it so the health reason is
    // legible. Require whichever variant is the active one.
    .required_programs(if fentry {
        &[("cpuacct_account_field_fentry", "CPU time by category")]
    } else {
        &[("cpuacct_account_field_kprobe", "CPU time by category")]
    })
    // The switch is read-only data the verifier folds at load (see
    // `task_attribution` in mod.bpf.c). Off, the per-task accounting still
    // runs; the per-task series and the events that label them do not exist.
    .pre_load(move |open| {
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .task_attribution = task_attribution as u8;
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .cgroup_attribution = cgroup_attribution as u8;
        if !cgroup_attribution {
            let maps = &mut open.maps;
            for map in [
                &mut maps.cgroup_info,
                &mut maps.cgroup_serial_numbers,
                &mut maps.cgroup_user,
                &mut maps.cgroup_system,
                &mut maps.cgroup_exited,
            ] {
                if let Err(e) = map.set_autocreate(false) {
                    debug!("{NAME}: could not leave a map out of the object: {e}");
                }
            }
        }
    });

    if cgroup_attribution {
        builder = builder
            .cgroup_counters(
                "cgroup_user",
                &CGROUP_CPU_USAGE_USER,
                &CGROUP_USAGE_ACQ,
                &CGROUP_IDENTITY,
            )
            .cgroup_counters(
                "cgroup_system",
                &CGROUP_CPU_USAGE_SYSTEM,
                &CGROUP_USAGE_ACQ,
                &CGROUP_IDENTITY,
            )
            .cgroup_counters(
                "cgroup_exited",
                &CGROUP_CPU_USAGE_EXITED,
                &CGROUP_EXITED_ACQ,
                &CGROUP_IDENTITY,
            )
            .ringbuf_handler("cgroup_info", handle_cgroup_info);
    }

    if task_attribution {
        builder = builder
            .sparse_packed_counters("task_cpu_usage", &TASK_CPU_USAGE, &TASK_USAGE_ACQ)
            .ringbuf_handler("task_info", handle_task_info)
            .ringbuf_handler("task_exit", handle_task_exit);
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
            "cgroup_user" => &self.maps.cgroup_user,
            "cgroup_system" => &self.maps.cgroup_system,
            "cgroup_exited" => &self.maps.cgroup_exited,
            "cpu_usage" => &self.maps.cpu_usage,
            "softirq" => &self.maps.softirq,
            "softirq_time" => &self.maps.softirq_time,
            "task_info" => &self.maps.task_info,
            "task_exit" => &self.maps.task_exit,
            "task_cpu_usage" => &self.maps.task_cpu_usage,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} cpuacct_account_field_fentry() BPF instruction count: {}",
            self.progs.cpuacct_account_field_fentry.insn_cnt()
        );
        debug!(
            "{NAME} cpuacct_account_field_kprobe() BPF instruction count: {}",
            self.progs.cpuacct_account_field_kprobe.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_process_exit_btf() BPF instruction count: {}",
            self.progs.handle__sched_process_exit_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_process_exit_raw() BPF instruction count: {}",
            self.progs.handle__sched_process_exit_raw.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_process_exec_btf() BPF instruction count: {}",
            self.progs.handle__sched_process_exec_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_process_exec_raw() BPF instruction count: {}",
            self.progs.handle__sched_process_exec_raw.insn_cnt()
        );
        debug!(
            "{NAME} handle__task_rename_btf() BPF instruction count: {}",
            self.progs.handle__task_rename_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__task_rename_raw() BPF instruction count: {}",
            self.progs.handle__task_rename_raw.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken::group::UID_LABEL;

    fn task_info(pid: u32, comm: &str, ts: u64) -> Vec<u8> {
        let mut info = bpf::types::task_info {
            pid,
            tgid: pid,
            ts,
            ..Default::default()
        };
        info.comm[..comm.len()].copy_from_slice(comm.as_bytes());
        unsafe { plain::as_bytes(&info) }.to_vec()
    }

    fn task_exit(pid: u32, ts: u64) -> Vec<u8> {
        let exit = bpf::types::task_exit {
            pid,
            ts,
            ..Default::default()
        };
        unsafe { plain::as_bytes(&exit) }.to_vec()
    }

    /// The BPF side sends task_info again when a thread is renamed. This is
    /// the userspace half: the same labels keep the occupant, new labels on
    /// the live slot replace it.
    #[test]
    fn a_rename_is_a_new_occupant_of_the_same_slot() {
        // TASK_IDENTITY is process-global; no other test uses this pid.
        let pid = 4_000_001;
        let slot = pid_slot(pid as usize);

        handle_task_info(&task_info(pid, "bash", 1));
        let first = TASK_CPU_USAGE.load_metadata(slot).unwrap();
        assert_eq!(first["comm"], "bash");

        handle_task_info(&task_info(pid, "bash", 2));
        let again = TASK_CPU_USAGE.load_metadata(slot).unwrap();
        assert_eq!(again[UID_LABEL], first[UID_LABEL]);

        handle_task_info(&task_info(pid, "yes", 3));
        let renamed = TASK_CPU_USAGE.load_metadata(slot).unwrap();
        assert_eq!(renamed["comm"], "yes");
        assert_ne!(renamed[UID_LABEL], first[UID_LABEL]);

        handle_task_exit(&task_exit(pid, 4));
        assert!(TASK_CPU_USAGE.load_metadata(slot).is_none());
    }

    /// A non-leader exec queues the old leader's exit before the new
    /// occupant's task_info for the same pid, and a drain applies the
    /// task_info first. The older exit must leave the new labels alone.
    #[test]
    fn an_exit_older_than_the_slots_labels_does_not_release_them() {
        let pid = 4_000_002;
        let slot = pid_slot(pid as usize);

        handle_task_info(&task_info(pid, "python3", 10));
        handle_task_info(&task_info(pid, "true", 30));
        handle_task_exit(&task_exit(pid, 20));
        let labels = TASK_CPU_USAGE.load_metadata(slot).unwrap();
        assert_eq!(labels["comm"], "true");

        handle_task_exit(&task_exit(pid, 40));
        assert!(TASK_CPU_USAGE.load_metadata(slot).is_none());
    }
}
