//! On-demand AMD GPU PMU recording.
//!
//! Unlike HTTP endpoints, this source reads the host's AMD GPU hardware
//! performance counters directly via rocprofiler-sdk — the same mechanism the
//! always-on `gpu_amd_pmu` agent sampler uses. The recorder owns a
//! [`Rocprofiler`] handle whose background worker threads bracket short windows
//! to keep the per-WGP 32-bit counters unsaturated; each tick we read the
//! accumulated per-GPU totals and emit them as a `metriken_exposition` snapshot,
//! which then flows through the same msgpack -> parquet path as scraped data.
//!
//! The counter set and GPU selection are configurable (see [`PmuConfig`]); when
//! unspecified they default to the sampler's counter set and all detected GPUs.
//! Requires `CAP_PERFMON` and a working ROCm runtime on the recording host.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use metriken_exposition::{Counter, Snapshot, SnapshotV2};

use crate::agent::samplers::gpu::linux::amd::pmu::catalog;
use crate::agent::samplers::gpu::linux::amd::pmu::perf_level;
use crate::agent::samplers::gpu::linux::amd::pmu::Rocprofiler;

// Re-exported so the recorder run loop can parse/validate the configured level.
pub(crate) use crate::agent::samplers::gpu::linux::amd::pmu::PerfLevel;

/// Configuration for on-demand AMD GPU PMU recording.
#[derive(Clone, Debug, Default)]
pub struct PmuConfig {
    /// GPU indices to record. `None` records every detected GPU. Indices match
    /// the agent sampler's GPU numbering (rocprofiler agent enumeration order).
    pub gpus: Option<Vec<usize>>,
    /// Hardware counter (PMU event) names to record. Empty means the default
    /// set, which matches the `gpu_amd_pmu` sampler.
    pub events: Vec<String>,
    /// AMD GPU performance level to set before recording. `None` leaves the GPU
    /// power state untouched. Already validated/parsed from config.
    pub gpu_perf_level: Option<PerfLevel>,
}

impl PmuConfig {
    /// The effective counter names: the configured set, or the default set when
    /// none were configured.
    pub fn events(&self) -> Vec<String> {
        if self.events.is_empty() {
            catalog::DEFAULT_COUNTERS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            self.events.clone()
        }
    }
}

/// A live AMD GPU PMU source: a rocprofiler handle plus the resolved set of GPU
/// indices to publish and the metadata used to build each snapshot.
pub struct PmuSource {
    rocp: Rocprofiler,
    /// GPU indices to read and publish each tick.
    gpu_indices: Vec<usize>,
    /// The descriptions (metric name -> help text) for the recorded events,
    /// written to parquet metadata so the viewer can label them.
    descriptions: HashMap<String, String>,
    /// The host hardware summary (CPU/memory/GPU/NIC), captured at init and
    /// written to the parquet `systeminfo` metadata key. The agent serves the
    /// same JSON from `/systeminfo`; capturing it here makes a standalone PMU
    /// recording self-describing without an agent in the loop.
    systeminfo: Option<String>,
}

impl PmuSource {
    /// Initialize PMU recording for the given config. Returns `Ok(None)` when no
    /// AMD GPU / ROCm runtime is present, so the caller can treat PMU as simply
    /// unavailable rather than a hard error.
    pub fn new(config: &PmuConfig) -> Result<Option<Self>, String> {
        let events = config.events();
        let event_refs: Vec<&str> = events.iter().map(|s| s.as_str()).collect();

        // Pin the GPU performance level before rocprofiler init, so the counting
        // contexts are armed while the GPU is already in the requested state. On
        // RDNA the per-SIMD counters only accumulate in a stable power state.
        // Apply to the configured GPUs, or all GPUs when none were selected.
        if let Some(level) = config.gpu_perf_level {
            match &config.gpus {
                Some(gpus) => {
                    perf_level::apply(level, gpus);
                }
                None => {
                    perf_level::apply_all(level);
                }
            }
        }

        let rocp = match Rocprofiler::new(&event_refs)? {
            Some(r) => r,
            None => return Ok(None),
        };

        let num_agents = rocp.num_agents();
        let gpu_indices = match &config.gpus {
            Some(sel) => {
                let mut out = Vec::new();
                for &idx in sel {
                    if idx >= num_agents {
                        return Err(format!(
                            "requested GPU {idx} but only {num_agents} GPU(s) were detected"
                        ));
                    }
                    out.push(idx);
                }
                if out.is_empty() {
                    return Err("no GPUs selected for PMU recording".into());
                }
                out
            }
            None => (0..num_agents).collect(),
        };

        // Build the descriptions map from the catalog for the events we know.
        let mut descriptions = HashMap::new();
        for event in &events {
            if let Some(entry) = catalog::lookup(event) {
                descriptions.insert(entry.metric.to_string(), entry.description.to_string());
            }
        }

        // Capture the host system summary, the same JSON the agent serves from
        // `/systeminfo`. Best-effort: on failure the recording simply omits it.
        let systeminfo = systeminfo::summary()
            .and_then(|s| serde_json::to_string(&s).ok())
            .or_else(|| {
                crate::debug!("gpu_amd_pmu (record): systeminfo unavailable");
                None
            });

        Ok(Some(Self {
            rocp,
            gpu_indices,
            descriptions,
            systeminfo,
        }))
    }

    /// The host system summary JSON, for the parquet `systeminfo` metadata key.
    pub fn systeminfo_json(&self) -> Option<String> {
        self.systeminfo.clone()
    }

    /// JSON map of metric name -> description, for the parquet `descriptions`
    /// metadata key.
    pub fn descriptions_json(&self) -> Option<String> {
        if self.descriptions.is_empty() {
            None
        } else {
            serde_json::to_string(&self.descriptions).ok()
        }
    }

    /// Read the current per-GPU counter totals and build a snapshot. Each GPU's
    /// counters are emitted as monotonic counters carrying `vendor`, `counter`,
    /// and `id` (GPU index) metadata — matching the sampler's metric labels so a
    /// recording is indistinguishable from a scraped agent.
    pub fn snapshot(&self) -> Snapshot {
        let mut counters: Vec<Counter> = Vec::new();

        for &idx in &self.gpu_indices {
            let sums = match self.rocp.sample(idx) {
                Ok(s) => s,
                Err(e) => {
                    crate::debug!("gpu_amd_pmu (record): GPU {idx}: read failed: {e}");
                    continue;
                }
            };

            for (counter_name, &value) in &sums {
                let entry = match catalog::lookup(counter_name) {
                    Some(e) => e,
                    // An event with no catalog mapping is still recorded under
                    // its raw counter name so nothing is silently dropped.
                    None => {
                        let mut metadata = HashMap::new();
                        metadata.insert("vendor".to_string(), "amd".to_string());
                        metadata.insert("counter".to_string(), counter_name.clone());
                        metadata.insert("id".to_string(), idx.to_string());
                        counters.push(Counter {
                            name: format!("gpmu_{}", counter_name.to_lowercase()),
                            value: clamp_u64(value),
                            metadata,
                        });
                        continue;
                    }
                };

                let mut metadata = HashMap::new();
                metadata.insert("vendor".to_string(), "amd".to_string());
                metadata.insert("counter".to_string(), entry.counter.to_string());
                metadata.insert("id".to_string(), idx.to_string());
                counters.push(Counter {
                    name: entry.metric.to_string(),
                    value: clamp_u64(value),
                    metadata,
                });
            }
        }

        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::now(),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }
}

/// The accumulated totals are non-negative integer counts delivered as f64.
fn clamp_u64(v: f64) -> u64 {
    if v.is_finite() && v >= 0.0 {
        v as u64
    } else {
        0
    }
}
