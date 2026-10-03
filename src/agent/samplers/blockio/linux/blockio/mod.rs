//! Collects block IO request counts, sizes, errors and latencies using BPF and
//! traces:
//! * `block_rq_complete`
//! * `block_rq_requeue`
//!
//! And produces these stats:
//! * `blockio_bytes`, `blockio_operations`, `blockio_size`, `blockio_errors`
//!   (labeled by `op` and `error` class) and `blockio_requeues` (part
//!   `requests`)
//! * `blockio_queue_latency`, `blockio_device_latency` and
//!   `blockio_total_latency` (part `latency`), read from the kernel's own
//!   per-request timestamps at completion (see
//!   docs/journal/2026-09-03-blockio-latency-rq-fields.md)
//!
//! One program per hook: this sampler replaced `blockio_requests` and
//! `blockio_latency`, which each had a program on `block_rq_complete`
//! (docs/journal/2026-10-03-one-program-per-hook.md). The parts are the
//! config options `requests` and `latency`, both on by default; a config that
//! still names the old samplers is translated at load (`Config::load`).

const NAME: &str = "blockio";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_blockio.bpf.rs"));
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

    let requests = config.part(NAME, "requests");
    let latency = config.part(NAME, "latency");
    if !requests && !latency {
        return Ok(None);
    }

    // A part that is off backs none of its metrics, so its groups have no
    // members; bound them, or every snapshot would carry them empty (the
    // sampler is live, so `bound_groups_without_a_live_sampler` skips them).
    if !requests {
        for group in [&COUNTERS_ACQ, &ERRORS_ACQ, &REQUEUES_ACQ, &SIZES_ACQ] {
            group.set_member_bound(0);
        }
    }
    if !latency {
        for group in [
            &DEVICE_LATENCIES_ACQ,
            &QUEUE_LATENCIES_ACQ,
            &TOTAL_LATENCIES_ACQ,
        ] {
            group.set_member_bound(0);
        }
    }

    let counters = vec![
        &BLOCKIO_READ_OPS,
        &BLOCKIO_WRITE_OPS,
        &BLOCKIO_FLUSH_OPS,
        &BLOCKIO_DISCARD_OPS,
        &BLOCKIO_READ_BYTES,
        &BLOCKIO_WRITE_BYTES,
        &BLOCKIO_FLUSH_BYTES,
        &BLOCKIO_DISCARD_BYTES,
    ];

    // Order MUST match the BPF layout:
    // errors[cpu * 32 + op * 7 + cls]
    //   op:  0=read, 1=write, 2=flush, 3=discard
    //   cls: 0=io, 1=timeout, 2=nospc, 3=target, 4=protection,
    //        5=unsupported, 6=other
    let errors = vec![
        // op = read
        &BLOCKIO_READ_ERR_IO,
        &BLOCKIO_READ_ERR_TIMEOUT,
        &BLOCKIO_READ_ERR_NOSPC,
        &BLOCKIO_READ_ERR_TARGET,
        &BLOCKIO_READ_ERR_PROTECTION,
        &BLOCKIO_READ_ERR_UNSUPPORTED,
        &BLOCKIO_READ_ERR_OTHER,
        // op = write
        &BLOCKIO_WRITE_ERR_IO,
        &BLOCKIO_WRITE_ERR_TIMEOUT,
        &BLOCKIO_WRITE_ERR_NOSPC,
        &BLOCKIO_WRITE_ERR_TARGET,
        &BLOCKIO_WRITE_ERR_PROTECTION,
        &BLOCKIO_WRITE_ERR_UNSUPPORTED,
        &BLOCKIO_WRITE_ERR_OTHER,
        // op = flush
        &BLOCKIO_FLUSH_ERR_IO,
        &BLOCKIO_FLUSH_ERR_TIMEOUT,
        &BLOCKIO_FLUSH_ERR_NOSPC,
        &BLOCKIO_FLUSH_ERR_TARGET,
        &BLOCKIO_FLUSH_ERR_PROTECTION,
        &BLOCKIO_FLUSH_ERR_UNSUPPORTED,
        &BLOCKIO_FLUSH_ERR_OTHER,
        // op = discard
        &BLOCKIO_DISCARD_ERR_IO,
        &BLOCKIO_DISCARD_ERR_TIMEOUT,
        &BLOCKIO_DISCARD_ERR_NOSPC,
        &BLOCKIO_DISCARD_ERR_TARGET,
        &BLOCKIO_DISCARD_ERR_PROTECTION,
        &BLOCKIO_DISCARD_ERR_UNSUPPORTED,
        &BLOCKIO_DISCARD_ERR_OTHER,
    ];

    // requeues[cpu * 8 + op]
    let requeues = vec![
        &BLOCKIO_READ_REQUEUE,
        &BLOCKIO_WRITE_REQUEUE,
        &BLOCKIO_FLUSH_REQUEUE,
        &BLOCKIO_DISCARD_REQUEUE,
    ];

    // One of each tp_btf/raw_tp twin, and no requeue program without requests.
    let mut disabled: Vec<&'static str> = if kernel_has_btf() {
        vec!["block_rq_complete_raw", "block_rq_requeue_raw"]
    } else {
        vec!["block_rq_complete_btf", "block_rq_requeue_btf"]
    };
    if !requests {
        disabled.extend(["block_rq_requeue_btf", "block_rq_requeue_raw"]);
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
    // `requests` and `latency` in mod.bpf.c); a part that is off has its maps
    // left out of the object and its series absent.
    .pre_load(move |open| {
        let rodata = open
            .maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data");
        rodata.requests = requests as u8;
        rodata.latency = latency as u8;

        let maps = &mut open.maps;
        let mut skip: Vec<&mut libbpf_rs::OpenMapMut<'_>> = Vec::new();
        if !requests {
            skip.extend([
                &mut maps.counters,
                &mut maps.errors,
                &mut maps.requeues,
                &mut maps.read_size,
                &mut maps.write_size,
                &mut maps.flush_size,
                &mut maps.discard_size,
            ]);
        }
        if !latency {
            skip.extend([
                &mut maps.read_device_latency,
                &mut maps.write_device_latency,
                &mut maps.flush_device_latency,
                &mut maps.discard_device_latency,
                &mut maps.read_queue_latency,
                &mut maps.write_queue_latency,
                &mut maps.flush_queue_latency,
                &mut maps.discard_queue_latency,
                &mut maps.read_total_latency,
                &mut maps.write_total_latency,
                &mut maps.flush_total_latency,
                &mut maps.discard_total_latency,
            ]);
        }
        for map in skip {
            if let Err(e) = map.set_autocreate(false) {
                debug!("{NAME}: could not leave a map out of the object: {e}");
            }
        }
    });

    if requests {
        builder = builder
            .counters("counters", counters, &COUNTERS_ACQ)
            .counters("errors", errors, &ERRORS_ACQ)
            .counters("requeues", requeues, &REQUEUES_ACQ)
            // All 4 op-class size histograms share ONE group — see stats.rs's
            // `SIZES_ACQ` doc comment.
            .histogram("read_size", &BLOCKIO_READ_SIZE, &SIZES_ACQ)
            .histogram("write_size", &BLOCKIO_WRITE_SIZE, &SIZES_ACQ)
            .histogram("flush_size", &BLOCKIO_FLUSH_SIZE, &SIZES_ACQ)
            .histogram("discard_size", &BLOCKIO_DISCARD_SIZE, &SIZES_ACQ);
    }

    if latency {
        builder = builder
            // One group per phase — see stats.rs's acquisition-group doc comment.
            .histogram(
                "read_device_latency",
                &BLOCKIO_READ_DEVICE_LATENCY,
                &DEVICE_LATENCIES_ACQ,
            )
            .histogram(
                "write_device_latency",
                &BLOCKIO_WRITE_DEVICE_LATENCY,
                &DEVICE_LATENCIES_ACQ,
            )
            .histogram(
                "flush_device_latency",
                &BLOCKIO_FLUSH_DEVICE_LATENCY,
                &DEVICE_LATENCIES_ACQ,
            )
            .histogram(
                "discard_device_latency",
                &BLOCKIO_DISCARD_DEVICE_LATENCY,
                &DEVICE_LATENCIES_ACQ,
            )
            .histogram(
                "read_queue_latency",
                &BLOCKIO_READ_QUEUE_LATENCY,
                &QUEUE_LATENCIES_ACQ,
            )
            .histogram(
                "write_queue_latency",
                &BLOCKIO_WRITE_QUEUE_LATENCY,
                &QUEUE_LATENCIES_ACQ,
            )
            .histogram(
                "flush_queue_latency",
                &BLOCKIO_FLUSH_QUEUE_LATENCY,
                &QUEUE_LATENCIES_ACQ,
            )
            .histogram(
                "discard_queue_latency",
                &BLOCKIO_DISCARD_QUEUE_LATENCY,
                &QUEUE_LATENCIES_ACQ,
            )
            .histogram(
                "read_total_latency",
                &BLOCKIO_READ_TOTAL_LATENCY,
                &TOTAL_LATENCIES_ACQ,
            )
            .histogram(
                "write_total_latency",
                &BLOCKIO_WRITE_TOTAL_LATENCY,
                &TOTAL_LATENCIES_ACQ,
            )
            .histogram(
                "flush_total_latency",
                &BLOCKIO_FLUSH_TOTAL_LATENCY,
                &TOTAL_LATENCIES_ACQ,
            )
            .histogram(
                "discard_total_latency",
                &BLOCKIO_DISCARD_TOTAL_LATENCY,
                &TOTAL_LATENCIES_ACQ,
            );
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
            "errors" => &self.maps.errors,
            "requeues" => &self.maps.requeues,
            "read_size" => &self.maps.read_size,
            "write_size" => &self.maps.write_size,
            "flush_size" => &self.maps.flush_size,
            "discard_size" => &self.maps.discard_size,
            "read_device_latency" => &self.maps.read_device_latency,
            "write_device_latency" => &self.maps.write_device_latency,
            "flush_device_latency" => &self.maps.flush_device_latency,
            "discard_device_latency" => &self.maps.discard_device_latency,
            "read_queue_latency" => &self.maps.read_queue_latency,
            "write_queue_latency" => &self.maps.write_queue_latency,
            "flush_queue_latency" => &self.maps.flush_queue_latency,
            "discard_queue_latency" => &self.maps.discard_queue_latency,
            "read_total_latency" => &self.maps.read_total_latency,
            "write_total_latency" => &self.maps.write_total_latency,
            "flush_total_latency" => &self.maps.flush_total_latency,
            "discard_total_latency" => &self.maps.discard_total_latency,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} block_rq_complete_btf() BPF instruction count: {}",
            self.progs.block_rq_complete_btf.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_complete_raw() BPF instruction count: {}",
            self.progs.block_rq_complete_raw.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_requeue_btf() BPF instruction count: {}",
            self.progs.block_rq_requeue_btf.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_requeue_raw() BPF instruction count: {}",
            self.progs.block_rq_requeue_raw.insn_cnt()
        );
    }
}
