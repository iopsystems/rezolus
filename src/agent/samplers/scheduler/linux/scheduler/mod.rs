//! Collects scheduler stats using BPF and traces:
//! * `sched_wakeup`
//! * `sched_wakeup_new`
//! * `sched_switch`
//!
//! And produces these stats:
//! * `scheduler_runqueue_latency`, `scheduler_running`, `scheduler_offcpu`,
//!   `scheduler_context_switch` and their per-cgroup series (part `runqueue`)
//! * `cpu_migrations` and `cgroup_cpu_migrations` (part `migrations`)
//!
//! One program per hook: this sampler replaced `scheduler_runqueue` and
//! `cpu_migrations`, which each had a program on `sched_switch`
//! (docs/journal/2026-10-03-one-program-per-hook.md). The parts are the
//! config options `runqueue` and `migrations`, both on by default; a config
//! that still names the old samplers is translated at load (`Config::load`).

const NAME: &str = "scheduler";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/scheduler_scheduler.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// The metrics a cgroup id reaches, one list per acquisition group.
///
/// One id spans several groups, and each group's schema carries its own copy
/// of the labels, so each group's metrics need their own entry. A part that
/// is off backs none of its metrics, so its groups are left out: there is one
/// identity, and one handler, per combination of parts.
static CGROUP_IDENTITY_BOTH: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(&[
        &[&CGROUP_SCHEDULER_IVCSW, &CGROUP_SCHEDULER_VCSW],
        &[&CGROUP_SCHEDULER_OFFCPU],
        &[&CGROUP_SCHEDULER_RUNQUEUE_WAIT],
        &[&CGROUP_CPU_MIGRATIONS],
    ]);

static CGROUP_IDENTITY_RUNQUEUE: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(&[
        &[&CGROUP_SCHEDULER_IVCSW, &CGROUP_SCHEDULER_VCSW],
        &[&CGROUP_SCHEDULER_OFFCPU],
        &[&CGROUP_SCHEDULER_RUNQUEUE_WAIT],
    ]);

static CGROUP_IDENTITY_MIGRATIONS: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(&[&[&CGROUP_CPU_MIGRATIONS]]);

fn handle_cgroup_info_both(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY_BOTH)
}

fn handle_cgroup_info_runqueue(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY_RUNQUEUE)
}

fn handle_cgroup_info_migrations(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY_MIGRATIONS)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let runqueue = config.part(NAME, "runqueue");
    let migrations = config.part(NAME, "migrations");
    if !runqueue && !migrations {
        return Ok(None);
    }

    // A part that is off backs none of its metrics, so its groups have no
    // members; bound them, or every snapshot would carry them empty (the
    // sampler is live, so `bound_groups_without_a_live_sampler` skips them).
    if !runqueue {
        for group in [
            &COUNTERS_ACQ,
            &RUNQLAT_ACQ,
            &RUNNING_ACQ,
            &OFFCPU_ACQ,
            &CGROUP_WAIT_ACQ,
            &CGROUP_OFFCPU_ACQ,
            &CGROUP_CONTEXT_SWITCH_ACQ,
        ] {
            group.set_member_bound(0);
        }
    }
    if !migrations {
        for group in [&MIGRATIONS_ACQ, &CGROUP_MIGRATIONS_ACQ] {
            group.set_member_bound(0);
        }
    }

    // order must match the counter positions in mod.bpf.c
    let counters = vec![
        &SCHEDULER_IVCSW,
        &SCHEDULER_RUNQUEUE_WAIT,
        &SCHEDULER_DISCARDED,
        &SCHEDULER_VCSW,
    ];

    let migration_counters = vec![&CPU_MIGRATIONS_FROM, &CPU_MIGRATIONS_TO];

    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);

    // One of each tp_btf/raw_tp twin, and no wakeup programs without the
    // runqueue part.
    let mut disabled: Vec<&'static str> = if kernel_has_btf() {
        vec![
            "handle__sched_wakeup_raw",
            "handle__sched_wakeup_new_raw",
            "handle__sched_switch_raw",
        ]
    } else {
        vec![
            "handle__sched_wakeup_btf",
            "handle__sched_wakeup_new_btf",
            "handle__sched_switch_btf",
        ]
    };
    if !runqueue {
        disabled.extend([
            "handle__sched_wakeup_btf",
            "handle__sched_wakeup_raw",
            "handle__sched_wakeup_new_btf",
            "handle__sched_wakeup_new_raw",
        ]);
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
    .disabled_programs(&disabled)
    // The switches are read-only data the verifier folds at load (see
    // `runqueue`, `migrations` and `cgroup_attribution` in mod.bpf.c); a part
    // that is off has its maps left out of the object and its series absent.
    .pre_load(move |open| {
        let rodata = open
            .maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data");
        rodata.runqueue = runqueue as u8;
        rodata.migrations = migrations as u8;
        rodata.cgroup_attribution = cgroup_attribution as u8;

        let maps = &mut open.maps;
        let mut skip: Vec<&mut libbpf_rs::OpenMapMut<'_>> = Vec::new();
        if !runqueue {
            skip.extend([
                &mut maps.counters,
                &mut maps.enqueued_at,
                &mut maps.offcpu_at,
                &mut maps.running_at,
                &mut maps.runqlat,
                &mut maps.running,
                &mut maps.offcpu,
                &mut maps.cgroup_ivcsw,
                &mut maps.cgroup_vcsw,
                &mut maps.cgroup_runq_wait,
                &mut maps.cgroup_offcpu,
            ]);
        } else if !cgroup_attribution {
            skip.extend([
                &mut maps.cgroup_ivcsw,
                &mut maps.cgroup_vcsw,
                &mut maps.cgroup_runq_wait,
                &mut maps.cgroup_offcpu,
            ]);
        }
        if !migrations {
            skip.extend([
                &mut maps.migrations_counts,
                &mut maps.cgroup_cpu_migrations,
                &mut maps.last_cpu,
            ]);
        } else if !cgroup_attribution {
            skip.push(&mut maps.cgroup_cpu_migrations);
        }
        if !cgroup_attribution {
            skip.extend([&mut maps.cgroup_info, &mut maps.cgroup_serial_numbers]);
        }
        for map in skip {
            if let Err(e) = map.set_autocreate(false) {
                debug!("{NAME}: could not leave a map out of the object: {e}");
            }
        }
    });

    if runqueue {
        builder = builder
            .cpu_counters("counters", counters, &COUNTERS_ACQ)
            .histogram("runqlat", &SCHEDULER_RUNQUEUE_LATENCY, &RUNQLAT_ACQ)
            .histogram("running", &SCHEDULER_RUNNING, &RUNNING_ACQ)
            .histogram("offcpu", &SCHEDULER_OFFCPU, &OFFCPU_ACQ);
    }

    if migrations {
        builder = builder.cpu_counters("migrations_counts", migration_counters, &MIGRATIONS_ACQ);
    }

    if cgroup_attribution {
        if runqueue {
            builder = builder
                .packed_counters(
                    "cgroup_runq_wait",
                    &CGROUP_SCHEDULER_RUNQUEUE_WAIT,
                    &CGROUP_WAIT_ACQ,
                )
                .packed_counters(
                    "cgroup_offcpu",
                    &CGROUP_SCHEDULER_OFFCPU,
                    &CGROUP_OFFCPU_ACQ,
                )
                .packed_counters(
                    "cgroup_ivcsw",
                    &CGROUP_SCHEDULER_IVCSW,
                    &CGROUP_CONTEXT_SWITCH_ACQ,
                )
                .packed_counters(
                    "cgroup_vcsw",
                    &CGROUP_SCHEDULER_VCSW,
                    &CGROUP_CONTEXT_SWITCH_ACQ,
                );
        }
        if migrations {
            builder = builder.packed_counters(
                "cgroup_cpu_migrations",
                &CGROUP_CPU_MIGRATIONS,
                &CGROUP_MIGRATIONS_ACQ,
            );
        }
        let handler: fn(&[u8]) -> i32 = match (runqueue, migrations) {
            (true, true) => handle_cgroup_info_both,
            (true, false) => handle_cgroup_info_runqueue,
            _ => handle_cgroup_info_migrations,
        };
        builder = builder.ringbuf_handler("cgroup_info", handler);
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
            "counters" => &self.maps.counters,
            "offcpu" => &self.maps.offcpu,
            "running" => &self.maps.running,
            "runqlat" => &self.maps.runqlat,
            "cgroup_runq_wait" => &self.maps.cgroup_runq_wait,
            "cgroup_offcpu" => &self.maps.cgroup_offcpu,
            "cgroup_ivcsw" => &self.maps.cgroup_ivcsw,
            "cgroup_vcsw" => &self.maps.cgroup_vcsw,
            "cgroup_info" => &self.maps.cgroup_info,
            "migrations_counts" => &self.maps.migrations_counts,
            "cgroup_cpu_migrations" => &self.maps.cgroup_cpu_migrations,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} handle__sched_switch_btf() BPF instruction count: {}",
            self.progs.handle__sched_switch_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_switch_raw() BPF instruction count: {}",
            self.progs.handle__sched_switch_raw.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_btf() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_raw() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_raw.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new_btf() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new_btf.insn_cnt()
        );
        debug!(
            "{NAME} handle__sched_wakeup_new_raw() BPF instruction count: {}",
            self.progs.handle__sched_wakeup_new_raw.insn_cnt()
        );
    }
}
