//! Collects Network Traffic stats using BPF and traces:
//! * `netif_receive_skb`
//! * `netdev_start_xmit`
//!
//! And produces these stats:
//! * `network/receive/bytes`
//! * `network/receive/frames`
//! * `network/transmit/bytes`
//! * `network/transmit/frames`
//!
//! Plus a host-boundary (north/south) pair of each, counted once at the
//! interface bound to a device driver instead of once per netdev traversed:
//! * `network_host_bytes`
//! * `network_host_packets`

const NAME: &str = "network_traffic";

#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/network_traffic.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Order is the BPF map layout, not taste: `Counters` reads slot `i` of each
    // CPU's bank into `counters[i]`, so this vec must match the `RX_BYTES` ..
    // `TX_HOST_PACKETS` indices in `mod.bpf.c` exactly.
    let counters = vec![
        &NETWORK_RX_BYTES,
        &NETWORK_TX_BYTES,
        &NETWORK_RX_PACKETS,
        &NETWORK_TX_PACKETS,
        &NETWORK_RX_HOST_BYTES,
        &NETWORK_TX_HOST_BYTES,
        &NETWORK_RX_HOST_PACKETS,
        &NETWORK_TX_HOST_PACKETS,
    ];

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .counters("counters", counters, &COUNTERS_ACQ)
    // BTF-typed attach where the kernel has BTF, probe-read attach where it
    // does not. The pair is identical in what it counts and differs only in
    // how it reaches the kernel structs: `tp_btf` compiles a field access to a
    // direct load, `raw_tp` to a `bpf_probe_read_kernel` CALL. On a probe
    // whose whole body is ~45 ns, that is most of the cost (#1218).
    //
    // Same shape `cpu_migrations` uses for `sched_switch`.
    .disabled_programs(if kernel_has_btf() {
        &["netif_receive_skb_raw", "net_dev_start_xmit_raw"]
    } else {
        &["netif_receive_skb_btf", "net_dev_start_xmit_btf"]
    })
    .build()?;

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
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        // Both flavors are reported: only one is attached, and which one is
        // the first thing to check when this sampler's cost looks wrong.
        debug!(
            "{NAME} netif_receive_skb_btf() BPF instruction count: {}",
            self.progs.netif_receive_skb_btf.insn_cnt()
        );
        debug!(
            "{NAME} net_dev_start_xmit_btf() BPF instruction count: {}",
            self.progs.net_dev_start_xmit_btf.insn_cnt()
        );
        debug!(
            "{NAME} netif_receive_skb_raw() BPF instruction count: {}",
            self.progs.netif_receive_skb_raw.insn_cnt()
        );
        debug!(
            "{NAME} net_dev_start_xmit_raw() BPF instruction count: {}",
            self.progs.net_dev_start_xmit_raw.insn_cnt()
        );
    }
}
