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

// Static slice of metrics that track cgroup-specific data
static CGROUP_METRICS: &[&dyn MetricGroup] = &[&CGROUP_CPU_MIGRATIONS];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, CGROUP_METRICS)
}

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Set metadata for root cgroup
    for metric in CGROUP_METRICS {
        metric.insert_metadata(1, "name".to_string(), "/".to_string());
    }

    let migrations = vec![&CPU_MIGRATIONS_FROM, &CPU_MIGRATIONS_TO];

    let bpf = BpfBuilder::new(
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .cpu_counters("migrations", migrations)
    .packed_counters("cgroup_cpu_migrations", &CGROUP_CPU_MIGRATIONS)
    .ringbuf_handler("cgroup_info", handle_cgroup_info)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
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
            "{NAME} handle__sched_switch() BPF instruction count: {}",
            self.progs.handle__sched_switch.insn_cnt()
        );
    }
}
