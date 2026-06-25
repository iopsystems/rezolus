//! Canonical catalog of AMD GPU PMU counters: the default event set plus the
//! mapping from each hardware counter name to the Rezolus metric it produces.
//!
//! This is the single source of truth shared by the always-on `gpu_amd_pmu`
//! sampler (whose statically-registered metrics in `stats.rs` mirror this table)
//! and the on-demand recorder (which builds `metriken_exposition` snapshots
//! directly from this table). Keep the two in sync: every entry here should have
//! a matching `#[metric]` in `stats.rs`, and vice versa.
//!
//! Counter names follow the RDNA (gfx12xx) naming; see
//! `docs/amd_gpu_pmu_events.md`.

/// One PMU counter and the Rezolus metric it maps to.
pub struct CounterMetric {
    /// The hardware counter name passed to rocprofiler (e.g. `"SQ_WAVES"`).
    pub counter: &'static str,
    /// The Rezolus metric name (e.g. `"gpmu_waves"`).
    pub metric: &'static str,
    /// Human-readable description, written to parquet metadata.
    pub description: &'static str,
}

/// The full catalog of PMU counters the sampler/recorder know how to map to
/// metrics. The default single-pass set ([`DEFAULT_COUNTERS`]) is a subset of
/// these; an operator may request any subset of these counter names.
pub const CATALOG: &[CounterMetric] = &[
    // ----- GRBM: front-end clocks (GPU busy) -----
    CounterMetric {
        counter: "GRBM_COUNT",
        metric: "gpmu_clock",
        description: "Wall clock cycles elapsed",
    },
    CounterMetric {
        counter: "GRBM_GUI_ACTIVE",
        metric: "gpmu_active_clock",
        description: "Wall clock at least one active wave in GPU",
    },
    // ----- SQ: waves, busy cycles, instruction mix -----
    CounterMetric {
        counter: "SQ_WAVES",
        metric: "gpmu_waves",
        description: "Wavefronts dispatched",
    },
    CounterMetric {
        counter: "SQ_BUSY_CYCLES",
        metric: "gpmu_busy_cycles",
        description: "Total active cycles of all Workgroup processors",
    },
    CounterMetric {
        counter: "SQ_WAVE_CYCLES",
        metric: "gpmu_wave_cycles",
        description: "The number of residence waves * active cycles of all Workgroup processors",
    },
    CounterMetric {
        counter: "SQ_INSTS_VALU",
        metric: "gpmu_valu_instructions",
        description: "Total Vector-ALU instructions issued by all Workgroup processors",
    },
    CounterMetric {
        counter: "SQ_INSTS_SALU",
        metric: "gpmu_salu_instructions",
        description: "Total Scalar-ALU instructions issued by all Workgroup processors",
    },
    CounterMetric {
        counter: "SQ_INSTS_LDS",
        metric: "gpmu_lds_instructions",
        description: "Total LDS (local data share) instructions issued by all Workgroup processors",
    },
    // ----- SQC: instruction cache -----
    CounterMetric {
        counter: "SQC_ICACHE_REQ",
        metric: "gpmu_icache_requests",
        description: "Instruction-cache requests",
    },
    CounterMetric {
        counter: "SQC_ICACHE_HITS",
        metric: "gpmu_icache_hits",
        description: "Instruction-cache hits",
    },
    // ----- GL2C: L2 cache + L2<->VRAM (memory bandwidth) -----
    CounterMetric {
        counter: "GL2C_EA_RDREQ",
        metric: "gpmu_vram_read_requests",
        description: "Total VRAM read requests from L2 cache",
    },
    CounterMetric {
        counter: "GL2C_EA_WRREQ",
        metric: "gpmu_vram_write_requests",
        description: "Total VRAM write requests from l2 cache",
    },
    CounterMetric {
        counter: "GL2C_HIT",
        metric: "gpmu_l2_hits",
        description: "L2 cache hits.",
    },
    CounterMetric {
        counter: "GL2C_MISS",
        metric: "gpmu_l2_misses",
        description: "L2 cache misses.",
    },
];

/// The default single-pass counter set, matching the always-on `gpu_amd_pmu`
/// sampler. These fit the RDNA per-block slot budget (SQ ≤ 8, GL2C ≤ 4, etc.).
///
/// SQ_WAVE_CYCLES is a per-WGP 32-bit accumulator that saturates within
/// ~34-275ms of busy time; the worker thread brackets each window with
/// start/stop (which resets the counters) so it stays unsaturated. See
/// `docs/amd_gpu_pmu_events.md` and `rocprofiler.rs`.
pub const DEFAULT_COUNTERS: &[&str] = &[
    "GRBM_COUNT",
    "GRBM_GUI_ACTIVE",
    "SQ_WAVES",
    "SQ_BUSY_CYCLES",
    "SQ_WAVE_CYCLES",
    "SQ_INSTS_VALU",
    "SQ_INSTS_SALU",
    "SQ_INSTS_LDS",
    "SQC_ICACHE_REQ",
    "SQC_ICACHE_HITS",
    "GL2C_EA_RDREQ",
    "GL2C_EA_WRREQ",
    "GL2C_HIT",
    "GL2C_MISS",
];

/// Look up the catalog entry for a hardware counter name, if known.
pub fn lookup(counter: &str) -> Option<&'static CounterMetric> {
    CATALOG.iter().find(|c| c.counter == counter)
}
