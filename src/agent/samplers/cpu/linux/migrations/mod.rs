//! Collects CPU migration stats using BPF and traces:
//! * `sched_migrate_task`
//!
//! And produces these stats:
//! * `cpu_migrations`
//! * `cpu_migrations_per_cpu`
//! * `cgroup_cpu_migrations`
//!
//! These stats can be used to understand process scheduling behavior and
//! identify potential performance issues due to excessive CPU migrations.

const NAME: &str = "cpu_migrations";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_migrations.bpf.rs"));
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
/// of the labels, so each group's metrics need their own entry.
static CGROUP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] =
    &[&[&CGROUP_CPU_MIGRATIONS]];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let migrations = vec![&CPU_MIGRATIONS_FROM, &CPU_MIGRATIONS_TO];

    let cgroup_attribution = config.cgroup_attribution_or(NAME, true);

    let mut builder = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .cpu_counters("migrations", migrations, &MIGRATIONS_ACQ)
    .disabled_programs(if kernel_has_btf() {
        &["handle__sched_switch_raw"]
    } else {
        &["handle__sched_switch_btf"]
    })
    // The switch is read-only data the verifier folds at load (see
    // `cgroup_attribution` in mod.bpf.c); the cgroup maps and their series
    // exist only when it is on.
    .pre_load(move |open| {
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .cgroup_attribution = cgroup_attribution as u8;
    });

    if cgroup_attribution {
        builder = builder
            .packed_counters(
                "cgroup_cpu_migrations",
                &CGROUP_CPU_MIGRATIONS,
                &CGROUP_MIGRATIONS_ACQ,
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
            "migrations" => &self.maps.migrations,
            "cgroup_cpu_migrations" => &self.maps.cgroup_cpu_migrations,
            "cgroup_info" => &self.maps.cgroup_info,
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
    }
}
