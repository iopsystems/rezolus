//! Collects CPU usage stats using BPF and traces:
//! * `cpuacct_account_field`
//! * `softirq_entry`
//! * `softirq_exit`
//! * `sched_process_exit`
//! * `sched_process_exec`
//! * `task_rename` — only with `task_attribution = true` or
//!   `process_attribution = true`
//!
//! And produces these stats:
//! * `cpu_usage`
//! * `cgroup_cpu_usage`
//! * `task_cpu_usage` — only with `task_attribution = true` (off by default)
//! * `process_cpu_usage` — only with `process_attribution = true` (off by
//!   default)
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

static PROCESS_METRICS: &[&dyn metriken::group::SlotMetadata] = &[&PROCESS_CPU_USAGE];

/// The only way this sampler writes what a process slot (`pid_slot(tgid)`)
/// means.
static PROCESS_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(PROCESS_IDENTITY_GROUPS);

static PROCESS_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[PROCESS_METRICS];

/// What each labelled process slot's labels came from: the newest `ts` of the
/// `process_info` events applied to it, and the start time and tgid of the
/// thread group they describe. Process slots have no exit event:
/// [`sweep_process_slots`] releases a slot once its `process_start_times`
/// marker no longer holds that group live, or the group has had its last
/// exit. Locked before [`DEAD_GROUPS`] wherever both are held.
#[derive(Clone, Copy)]
struct ProcessAssigned {
    ts: u64,
    start_time: u64,
    tgid: u32,
}

static PROCESS_ASSIGNED: std::sync::Mutex<std::collections::BTreeMap<usize, ProcessAssigned>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

/// What each labelled task slot's labels came from: the newest `ts` of the
/// `task_info` events applied to it, and the `start_time` and `tgid` of the
/// instance that event describes.
///
/// `task_info` and `task_exit` arrive on separate ringbufs, and one drain
/// applies every `task_info` before any `task_exit`. When a pid's old
/// occupant exits and its new occupant's `task_info` is sent in the same
/// snapshot interval (nearly always so for a thread that execs from outside
/// the group leader), the old occupant's exit is applied after the new
/// labels. An exit older than the slot's last assignment must not release
/// it.
///
/// What the events leave behind -- labels of an instance whose `task_exit`
/// was dropped, or a `task_info` for an exiting task stamped after its exit
/// -- is released by [`sweep_task_slots`].
#[derive(Clone, Copy)]
struct Assigned {
    ts: u64,
    start_time: u64,
    tgid: u32,
}

static ASSIGNED_AT: std::sync::Mutex<std::collections::BTreeMap<usize, Assigned>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// A drained `task_info` whose task was younger than [`MIN_LIFETIME_NS`]:
/// the labels it would have assigned, and what [`ASSIGNED_AT`] would have
/// recorded.
struct Pending {
    labels: std::collections::BTreeMap<String, String>,
    at: Assigned,
}

/// Labels held back from task slots until the task has lived
/// [`MIN_LIFETIME_NS`], by slot, with the newest `task_info` for each.
/// [`sweep_task_slots`] assigns them once the task is old enough, and drops
/// them, as it releases assigned labels, once the instance is gone. A
/// `task_exit` drops one no newer than the exit. Locked after
/// [`ASSIGNED_AT`] and before [`DEAD_GROUPS`] wherever they are held
/// together.
static PENDING: std::sync::Mutex<std::collections::BTreeMap<usize, Pending>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The config option `task_min_lifetime` in nanoseconds, set by `init`, or
/// 0 with `snapshot_format = "v2"`. A task is old enough when
/// `CLOCK_MONOTONIC` less its `start_time` (also `CLOCK_MONOTONIC`) is at
/// least this. The agent's own `CLOCK_MONOTONIC` is taken to be the
/// kernel's: in a time namespace with a monotonic offset, tasks look older
/// (positive offset) or younger (negative) by that offset.
static MIN_LIFETIME_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `CLOCK_MONOTONIC` in nanoseconds, the clock of `task->start_time` and
/// `bpf_ktime_get_ns()`.
fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Cannot fail for CLOCK_MONOTONIC with a valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Thread groups left with no live thread, from `task_exit.dead_tgid`: the
/// newest such exit's `ts`, and whether a sweep has already applied it.
/// Each entry is applied by two sweeps, because a `task_info` sent before
/// the group's last exit can be drained one drain after it: one drain reads
/// the `task_info` ring before the `task_exit` ring. [`age_dead_groups`],
/// the last sweep after each drain, ages the entries. Locked after
/// [`ASSIGNED_AT`], [`PENDING`] or [`PROCESS_ASSIGNED`] wherever they are
/// held together.
static DEAD_GROUPS: std::sync::Mutex<std::collections::BTreeMap<u32, (u64, bool)>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

// The bits of a task_start_times marker that are not the start time; see
// METADATA_PENDING and INSTANCE_EXITED in mod.bpf.c.
const METADATA_PENDING: u64 = 1 << 63;
const INSTANCE_EXITED: u64 = 1 << 62;

/// Release every labelled task slot whose instance the BPF side no longer
/// holds live: its `task_start_times` marker shows that instance exited, or
/// another instance, or none. Release too every slot labelled with the tgid
/// of a thread group that has since had its last exit (a thread that exec'd
/// from outside the leader and failed after de_thread leaves its old pid's
/// marker live); labels written after that exit belong to a new group with
/// the same tgid and are kept. Runs after every drain, before
/// [`age_dead_groups`], so labels the events left behind last at most one
/// snapshot interval.
///
/// A live marker for the slot's instance keeps the labels even when a newer
/// `task_info` is still in flight (a thread that exec'd from outside the
/// group leader runs under the leader's start_time); that `task_info`
/// replaces them when it is drained.
///
/// A `task_info` is submitted only after its instance's marker is written
/// (pending first; see handle_new_task in mod.bpf.c), so a drained
/// `task_info` never meets an older marker.
///
/// Labels held in [`PENDING`] are dropped by the same test, and assigned
/// once their task has lived [`MIN_LIFETIME_NS`].
fn sweep_task_slots(markers: &[std::sync::atomic::AtomicU64]) {
    sweep_task_slots_at(
        markers,
        MIN_LIFETIME_NS.load(std::sync::atomic::Ordering::Relaxed),
        monotonic_ns(),
    );
}

fn sweep_task_slots_at(markers: &[std::sync::atomic::AtomicU64], min_lifetime: u64, now: u64) {
    let mut assigned = ASSIGNED_AT.lock().unwrap_or_else(|e| e.into_inner());
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let dead_groups = DEAD_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
    let holds = |slot: usize, at: &Assigned| {
        let marker = markers
            .get(slot)
            .map_or(0, |m| m.load(std::sync::atomic::Ordering::Relaxed));
        let live = marker & INSTANCE_EXITED == 0 && marker & !METADATA_PENDING == at.start_time;
        let group_gone = dead_groups
            .get(&at.tgid)
            .is_some_and(|&(ts, _)| ts >= at.ts);
        live && !group_gone
    };
    pending.retain(|&slot, p| {
        if !holds(slot, &p.at) {
            return false;
        }
        if now.saturating_sub(p.at.start_time) < min_lifetime {
            return true;
        }
        TASK_IDENTITY.assign(slot, std::mem::take(&mut p.labels));
        assigned.insert(slot, p.at);
        false
    });
    assigned.retain(|&slot, at| {
        if !holds(slot, at) {
            TASK_IDENTITY.release(slot);
            return false;
        }
        true
    });
}

/// Age [`DEAD_GROUPS`]: each entry is applied by the sweeps after the drain
/// that recorded it and after the next one, then dropped. Registered after
/// [`sweep_process_slots`] and [`sweep_task_slots`], whichever of them run.
/// It reads no map; `markers` is the map it is registered against.
fn age_dead_groups(_markers: &[std::sync::atomic::AtomicU64]) {
    let mut dead_groups = DEAD_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
    dead_groups.retain(|_, (_, applied)| !std::mem::replace(applied, true));
}

/// Release every labelled process slot whose thread group the BPF side no
/// longer holds live: its `process_start_times` marker shows that group
/// exited (its last thread has entered do_exit), another group, or none.
/// Release too every slot whose tgid is in [`DEAD_GROUPS`] from an exit
/// newer than its labels. The marker writes are not atomic: a
/// `set_task_comm` on a kernel thread by another task (5.10's
/// `kthread_create` renames the new thread from its creator's context; a
/// userspace rename is refused outside the thread group) whose send finds the
/// ringbuf full, racing that thread's exit, can leave the marker pending
/// after the exit marked it exited. Runs after every drain, before
/// [`age_dead_groups`], so each entry is applied here by two sweeps as
/// well. That needs one of the group's last `task_exit` events
/// delivered: with all of them dropped, labels that race leaves stay until a
/// new group takes the tgid's slot.
///
/// A `process_info` is submitted only after its group's marker is written
/// (pending first; see start_process in mod.bpf.c), so a drained
/// `process_info` never meets an older marker.
fn sweep_process_slots(markers: &[std::sync::atomic::AtomicU64]) {
    let mut assigned = PROCESS_ASSIGNED.lock().unwrap_or_else(|e| e.into_inner());
    let dead_groups = DEAD_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
    assigned.retain(|&slot, at| {
        let marker = markers
            .get(slot)
            .map_or(0, |m| m.load(std::sync::atomic::Ordering::Relaxed));
        let live = marker & INSTANCE_EXITED == 0 && marker & !METADATA_PENDING == at.start_time;
        let group_gone = dead_groups
            .get(&at.tgid)
            .is_some_and(|&(ts, _)| ts >= at.ts);
        if !live || group_gone {
            PROCESS_IDENTITY.release(slot);
            return false;
        }
        true
    });
}

/// The labels a `task_info` record carries: `tgid`, `comm` (when not empty)
/// and `cgroup`. A task's series adds `pid`.
fn info_labels(info: &bpf::types::task_info) -> std::collections::BTreeMap<String, String> {
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

    let mut labels = std::collections::BTreeMap::new();
    labels.insert("tgid".to_string(), info.tgid.to_string());
    if !comm.is_empty() {
        labels.insert("comm".to_string(), comm);
    }
    labels.insert("cgroup".to_string(), cgroup);
    labels
}

/// A thread group's metadata, sent when the group is first seen and when
/// its leader is renamed. The record describes the group leader; its
/// `start_time` is the group's.
fn handle_process_info(data: &[u8]) -> i32 {
    let mut info = bpf::types::task_info::default();

    if plain::copy_from_bytes(&mut info, data).is_ok() {
        let slot = pid_slot(info.tgid as usize);
        let mut assigned = PROCESS_ASSIGNED.lock().unwrap_or_else(|e| e.into_inner());
        PROCESS_IDENTITY.assign(slot, info_labels(&info));
        assigned.insert(
            slot,
            ProcessAssigned {
                ts: info.ts,
                start_time: info.start_time,
                tgid: info.tgid,
            },
        );
    }

    0
}

fn handle_task_info(data: &[u8]) -> i32 {
    let mut info = bpf::types::task_info::default();

    if plain::copy_from_bytes(&mut info, data).is_ok() {
        apply_task_info(
            &info,
            MIN_LIFETIME_NS.load(std::sync::atomic::Ordering::Relaxed),
            monotonic_ns(),
        );
    }

    0
}

/// Label the task's slot, or, for a task that has not yet lived
/// `min_lifetime` at `now`, hold the labels in [`PENDING`].
fn apply_task_info(info: &bpf::types::task_info, min_lifetime: u64, now: u64) {
    let pid = info.pid as usize;
    let slot = pid_slot(pid);

    // Built once and set once. Setting the four labels separately left a
    // window where a reader could see a slot half-way through changing
    // hands — the new task's pid beside the old task's comm. See
    // `agent::identity`.
    let mut labels = info_labels(info);
    labels.insert("pid".to_string(), pid.to_string());

    let this = Assigned {
        ts: info.ts,
        start_time: info.start_time,
        tgid: info.tgid,
    };

    let mut assigned = ASSIGNED_AT.lock().unwrap_or_else(|e| e.into_inner());
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if now.saturating_sub(info.start_time) < min_lifetime {
        if pending.get(&slot).is_none_or(|p| info.ts >= p.at.ts) {
            pending.insert(slot, Pending { labels, at: this });
        }
        return;
    }
    if pending.get(&slot).is_some_and(|p| p.at.ts <= info.ts) {
        pending.remove(&slot);
    }
    TASK_IDENTITY.assign(slot, labels);
    let at = assigned.entry(slot).or_insert(this);
    if info.ts >= at.ts {
        *at = this;
    }
}

fn handle_task_exit(data: &[u8]) -> i32 {
    let mut exit = bpf::types::task_exit::default();

    if plain::copy_from_bytes(&mut exit, data).is_ok() {
        let slot = pid_slot(exit.pid as usize);

        let mut assigned = ASSIGNED_AT.lock().unwrap_or_else(|e| e.into_inner());
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if pending.get(&slot).is_some_and(|p| p.at.ts <= exit.ts) {
            pending.remove(&slot);
        }
        if exit.dead_tgid != 0 {
            let mut dead_groups = DEAD_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
            let at = dead_groups
                .entry(exit.dead_tgid)
                .or_insert((exit.ts, false));
            *at = (at.0.max(exit.ts), false);
        }
        if assigned.get(&slot).is_some_and(|at| at.ts > exit.ts) {
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
    // A V2 snapshot lists every non-zero slot, labelled or not, so a held
    // task would be exported with only its `id`.
    let min_lifetime = match config.general().snapshot_format() {
        crate::agent::config::SnapshotFormat::V2 => std::time::Duration::ZERO,
        crate::agent::config::SnapshotFormat::V3 => config.task_min_lifetime(NAME),
    };
    MIN_LIFETIME_NS.store(
        u64::try_from(min_lifetime.as_nanos()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    let process_attribution = config.process_attribution(NAME);
    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);

    let fentry = kernel_can_fentry("cpuacct_account_field");
    let btf = kernel_has_btf();

    // BTF present: the tp_btf twin of each tracepoint program, else its
    // raw_tp twin. cpuacct_account_field: the fentry twin (cheaper dispatch)
    // where fentry can attach to it (`kernel_can_fentry`), else the kprobe.
    // task_rename relabels per-task and per-process series, so neither twin
    // loads without task_attribution or process_attribution.
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
    if !task_attribution && !process_attribution {
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
    // The switches are read-only data the verifier folds at load (see
    // `task_attribution` in mod.bpf.c). Off, the per-task accounting still
    // runs; the series and the events that label them do not exist.
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
            .process_attribution = process_attribution as u8;
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
        // 64 MiB of per-pid arrays only the per-process series use.
        if !process_attribution {
            let maps = &mut open.maps;
            for map in [
                &mut maps.process_cpu_usage,
                &mut maps.process_start_times,
                &mut maps.process_info,
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

    // Ringbufs are drained, and sweeps run, in registration order:
    // task_info before task_exit, both sweeps before age_dead_groups.
    if task_attribution {
        builder = builder
            .sparse_packed_counters("task_cpu_usage", &TASK_CPU_USAGE, &TASK_USAGE_ACQ)
            .ringbuf_handler("task_info", handle_task_info);
    }
    // With only process_attribution, task_exit carries just each group's
    // last exit: its dead_tgid lets sweep_process_slots release labels whose
    // marker a racing rename left pending after that exit (DEAD_GROUPS).
    if task_attribution || process_attribution {
        builder = builder.ringbuf_handler("task_exit", handle_task_exit);
    }
    if process_attribution {
        builder = builder
            .sparse_packed_counters("process_cpu_usage", &PROCESS_CPU_USAGE, &PROCESS_USAGE_ACQ)
            .ringbuf_handler("process_info", handle_process_info)
            .map_sweep("process_start_times", sweep_process_slots);
    }
    if task_attribution {
        builder = builder.map_sweep("task_start_times", sweep_task_slots);
    }
    if task_attribution || process_attribution {
        builder = builder.map_sweep("task_start_times", age_dead_groups);
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
            "task_start_times" => &self.maps.task_start_times,
            "process_cpu_usage" => &self.maps.process_cpu_usage,
            "process_start_times" => &self.maps.process_start_times,
            "process_info" => &self.maps.process_info,
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

    /// Held by every test that sweeps. A sweep walks the process-global
    /// `ASSIGNED_AT` against its caller's markers and `age_dead_groups` ages
    /// the entries in `DEAD_GROUPS`, so two at once release each other's
    /// labels and use up each other's dead-group entries.
    static SWEEPS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The task-side sweeps after one drain, in registration order.
    fn sweep_tasks(markers: &[std::sync::atomic::AtomicU64]) {
        sweep_task_slots(markers);
        age_dead_groups(markers);
    }

    fn task_info(pid: u32, comm: &str, ts: u64) -> Vec<u8> {
        task_info_of(pid, comm, ts, 0)
    }

    fn task_info_of(pid: u32, comm: &str, ts: u64, start_time: u64) -> Vec<u8> {
        task_info_in(pid, pid, comm, ts, start_time)
    }

    fn task_info_in(pid: u32, tgid: u32, comm: &str, ts: u64, start_time: u64) -> Vec<u8> {
        let info = info_in(pid, tgid, comm, ts, start_time);
        unsafe { plain::as_bytes(&info) }.to_vec()
    }

    fn info_in(pid: u32, tgid: u32, comm: &str, ts: u64, start_time: u64) -> bpf::types::task_info {
        let mut info = bpf::types::task_info {
            pid,
            tgid,
            ts,
            start_time,
            ..Default::default()
        };
        info.comm[..comm.len()].copy_from_slice(comm.as_bytes());
        info
    }

    fn task_exit(pid: u32, ts: u64) -> Vec<u8> {
        task_exit_of(pid, 0, ts)
    }

    fn task_exit_of(pid: u32, dead_tgid: u32, ts: u64) -> Vec<u8> {
        let exit = bpf::types::task_exit { pid, dead_tgid, ts };
        unsafe { plain::as_bytes(&exit) }.to_vec()
    }

    /// A process slot is labelled by its group's `process_info` with
    /// `tgid`, `comm` and `cgroup`, and no `pid`. A leader rename relabels
    /// it. The sweep keeps the labels while the marker holds that group live,
    /// pending or not, and releases them once the marker shows the group
    /// exited, another group, or none, or once the group has had a last exit
    /// newer than the labels.
    #[test]
    fn the_process_sweep_releases_labels_whose_group_is_gone() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // The only test that labels process slots or sweeps them. It records
        // a dead group, which age_dead_groups ages.
        let _sweeps = SWEEPS.lock().unwrap_or_else(|e| e.into_inner());
        let tgid = 3201;
        let slot = pid_slot(tgid as usize);
        let start = 9_000;
        let markers: Vec<AtomicU64> = (0..4096).map(|_| AtomicU64::new(0)).collect();
        let labels = || PROCESS_CPU_USAGE.load_metadata(slot);

        handle_process_info(&task_info_in(tgid, tgid, "bash", 1, start));
        let first = labels().unwrap();
        assert_eq!(first["tgid"], tgid.to_string());
        assert_eq!(first["comm"], "bash");
        assert_eq!(first["cgroup"], "/");
        assert!(!first.contains_key("pid"));

        handle_process_info(&task_info_in(tgid, tgid, "postgres", 2, start));
        let renamed = labels().unwrap();
        assert_eq!(renamed["comm"], "postgres");
        assert_ne!(renamed[UID_LABEL], first[UID_LABEL]);

        for (marker, kept) in [
            (start, true),
            (start | METADATA_PENDING, true),
            (start | INSTANCE_EXITED, false),
            (start + 1, false),
            (0, false),
        ] {
            handle_process_info(&task_info_in(tgid, tgid, "postgres", 3, start));
            markers[slot].store(marker, Ordering::Relaxed);
            sweep_process_slots(&markers);
            assert_eq!(labels().is_some(), kept, "marker {marker:#x}");
        }

        markers[slot].store(start | INSTANCE_EXITED, Ordering::Relaxed);
        sweep_process_slots(&markers);
        assert!(labels().is_none());

        // A marker left pending after the group's last exit: the exit's
        // dead_tgid releases labels older than it and keeps newer ones (a
        // new group with the same tgid).
        markers[slot].store(start | METADATA_PENDING, Ordering::Relaxed);
        handle_process_info(&task_info_in(tgid, tgid, "postgres", 10, start));
        handle_task_exit(&task_exit_of(tgid, tgid, 11));
        sweep_process_slots(&markers);
        assert!(labels().is_none());

        markers[slot].store(start + 1, Ordering::Relaxed);
        handle_process_info(&task_info_in(tgid, tgid, "postgres", 12, start + 1));
        sweep_process_slots(&markers);
        assert!(labels().is_some());
        markers[slot].store(0, Ordering::Relaxed);
        sweep_process_slots(&markers);
        assert!(labels().is_none());
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

    /// The sweep keeps labels whose instance the marker holds live, pending
    /// or not, and releases them once the marker shows that instance
    /// exited, another instance, or none.
    #[test]
    fn the_sweep_releases_labels_whose_instance_is_gone() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // A pid no other test uses, below the size of `markers`. The sweep
        // walks every labelled slot, including other tests'. Tests that do
        // not sweep keep their labels because they use start_time 0 and
        // slots outside `markers`, which read 0; tests that sweep hold
        // SWEEPS.
        let _sweeps = SWEEPS.lock().unwrap_or_else(|e| e.into_inner());
        let pid = 3001;
        let slot = pid_slot(pid as usize);
        let start = 5_000;
        let markers: Vec<AtomicU64> = (0..4096).map(|_| AtomicU64::new(0)).collect();
        let labelled = || TASK_CPU_USAGE.load_metadata(slot).is_some();

        for (marker, kept) in [
            (start, true),
            (start | METADATA_PENDING, true),
            (start | INSTANCE_EXITED, false),
            (start + 1, false),
            (0, false),
        ] {
            handle_task_info(&task_info_of(pid, "worker", 1, start));
            markers[slot].store(marker, Ordering::Relaxed);
            sweep_tasks(&markers);
            assert_eq!(labelled(), kept, "marker {marker:#x}");
            if kept {
                handle_task_exit(&task_exit(pid, 2));
            }
        }
        assert!(!labelled());
    }

    /// A thread whose exec fails after de_thread leaves its old pid's slot
    /// labelled with a live marker. The group's last exit carries the tgid,
    /// and the next sweep releases every slot labelled with it that is older
    /// than that exit; a slot labelled after it, for a new group that reuses
    /// the tgid, is kept.
    #[test]
    fn a_dead_groups_slots_are_released_by_the_next_sweep() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // Pids no other test uses, below the size of `markers`.
        let _sweeps = SWEEPS.lock().unwrap_or_else(|e| e.into_inner());
        let (tgid, orphan, newer) = (3101, 3102, 3103);
        let start = 7_000;
        let markers: Vec<AtomicU64> = (0..4096).map(|_| AtomicU64::new(0)).collect();
        let labelled = |pid: u32| {
            TASK_CPU_USAGE
                .load_metadata(pid_slot(pid as usize))
                .is_some()
        };

        // The orphan's marker stays live: its exit is never seen.
        handle_task_info(&task_info_in(orphan, tgid, "worker", 1, start));
        markers[pid_slot(orphan as usize)].store(start, Ordering::Relaxed);
        sweep_tasks(&markers);
        assert!(labelled(orphan));

        // The group's last thread exits under the leader's pid; a new group
        // with the same tgid is labelled after that exit.
        handle_task_exit(&task_exit_of(tgid, tgid, 5));
        handle_task_info(&task_info_in(newer, tgid, "worker", 6, start + 1));
        markers[pid_slot(newer as usize)].store(start + 1, Ordering::Relaxed);
        sweep_tasks(&markers);
        assert!(!labelled(orphan));
        assert!(labelled(newer));

        handle_task_exit(&task_exit(newer, 7));
        assert!(!labelled(newer));

        // A task_info sent before the group's last exit but drained one drain
        // after it is released by the next sweep; one drained two drains
        // after it is not (the entry is gone).
        let late = 3104;
        handle_task_exit(&task_exit_of(tgid, tgid, 20));
        sweep_tasks(&markers);
        handle_task_info(&task_info_in(late, tgid, "worker", 19, start + 2));
        markers[pid_slot(late as usize)].store(start + 2, Ordering::Relaxed);
        sweep_tasks(&markers);
        assert!(!labelled(late));
        handle_task_info(&task_info_in(late, tgid, "worker", 19, start + 2));
        sweep_tasks(&markers);
        assert!(labelled(late));
        handle_task_exit(&task_exit(late, 21));
        assert!(!labelled(late));
    }

    /// A task younger than the minimum lifetime gets no labels when its
    /// `task_info` is drained. The first sweep at which it has lived the
    /// minimum assigns them, with any rename since; a sweep before that, or
    /// an exit older than the held labels, leaves them held. A task older
    /// than the minimum is labelled at once, and a rename drained once it
    /// is old enough replaces the held labels.
    #[test]
    fn a_task_is_labelled_once_it_has_lived_the_minimum() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // Pids no other test uses, below the size of `markers`.
        let _sweeps = SWEEPS.lock().unwrap_or_else(|e| e.into_inner());
        let (pid, old) = (3301, 3302);
        let slot = pid_slot(pid as usize);
        let min = 1_000_000_000;
        let start = 10_000_000_000;
        let markers: Vec<AtomicU64> = (0..4096).map(|_| AtomicU64::new(0)).collect();
        let labels = || TASK_CPU_USAGE.load_metadata(slot);
        let sweep = |now| {
            sweep_task_slots_at(&markers, min, now);
            age_dead_groups(&markers);
        };

        markers[slot].store(start, Ordering::Relaxed);
        apply_task_info(
            &info_in(pid, pid, "worker", start + 1, start),
            min,
            start + 1,
        );
        assert!(labels().is_none());
        apply_task_info(
            &info_in(pid, pid, "pool-1", start + 2, start),
            min,
            start + 2,
        );
        handle_task_exit(&task_exit(pid, start + 1));
        sweep(start + min - 1);
        assert!(labels().is_none());
        sweep(start + min);
        assert_eq!(labels().unwrap()["comm"], "pool-1");
        handle_task_exit(&task_exit(pid, start + min + 1));
        assert!(labels().is_none());

        // Held, then renamed in the drain in which it reaches the minimum:
        // the rename is labelled at once and the sweep does not replace it
        // with the held labels.
        let start = start + 10 * min;
        markers[slot].store(start, Ordering::Relaxed);
        apply_task_info(
            &info_in(pid, pid, "worker", start + 1, start),
            min,
            start + 1,
        );
        apply_task_info(
            &info_in(pid, pid, "pool-2", start + min, start),
            min,
            start + min,
        );
        sweep(start + min);
        assert_eq!(labels().unwrap()["comm"], "pool-2");
        handle_task_exit(&task_exit(pid, start + min + 1));
        assert!(labels().is_none());

        let old_start = 1_000;
        let old_slot = pid_slot(old as usize);
        markers[old_slot].store(old_start, Ordering::Relaxed);
        apply_task_info(&info_in(old, old, "main", start, old_start), min, start);
        assert!(TASK_CPU_USAGE.load_metadata(old_slot).is_some());
        handle_task_exit(&task_exit(old, start + 1));
    }

    /// Held labels are dropped, never assigned, when their task exits
    /// before the minimum, when its marker shows it gone (an exit event
    /// that was dropped), or when its group has had its last exit since.
    /// The sweep drops them while the task would still be young, so they
    /// are not kept until the minimum.
    #[test]
    fn a_task_that_exits_before_the_minimum_is_never_labelled() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _sweeps = SWEEPS.lock().unwrap_or_else(|e| e.into_inner());
        let (exits, dropped) = (3303, 3304);
        let min = 1_000_000_000;
        let start = 20_000_000_000;
        let markers: Vec<AtomicU64> = (0..4096).map(|_| AtomicU64::new(0)).collect();
        let labelled = |pid: u32| {
            TASK_CPU_USAGE
                .load_metadata(pid_slot(pid as usize))
                .is_some()
        };

        for pid in [exits, dropped] {
            markers[pid_slot(pid as usize)].store(start, Ordering::Relaxed);
            apply_task_info(
                &info_in(pid, pid, "worker", start + 1, start),
                min,
                start + 1,
            );
        }
        let held = |pid: u32| {
            PENDING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&pid_slot(pid as usize))
        };
        handle_task_exit(&task_exit(exits, start + 2));
        assert!(!held(exits));
        markers[pid_slot(dropped as usize)].store(start | INSTANCE_EXITED, Ordering::Relaxed);
        sweep_task_slots_at(&markers, min, start + 3);
        age_dead_groups(&markers);
        assert!(!held(dropped));

        // Held with a live marker, in a group whose last exit came after
        // the labels (a thread whose exec failed after de_thread).
        let (tgid, orphan) = (3305, 3306);
        markers[pid_slot(orphan as usize)].store(start, Ordering::Relaxed);
        apply_task_info(
            &info_in(orphan, tgid, "worker", start + 1, start),
            min,
            start + 1,
        );
        handle_task_exit(&task_exit_of(tgid, tgid, start + 2));
        sweep_task_slots_at(&markers, min, start + 3);
        age_dead_groups(&markers);
        assert!(!held(orphan));
        age_dead_groups(&markers);
        sweep_task_slots_at(&markers, min, start + 2 * min);
        age_dead_groups(&markers);
        assert!(!labelled(exits));
        assert!(!labelled(dropped));
    }
}
