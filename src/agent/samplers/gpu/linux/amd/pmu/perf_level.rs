//! AMD GPU performance-level control for the `gpu_amd_pmu` sampler.
//!
//! On RDNA (gfx10/11/12) GPUs many per-SIMD PMU counters only accumulate when
//! the GPU is held in a **stable power state** (a documented rocprofiler
//! requirement). The agent does not change the power state by default — pinning
//! clocks perturbs real workloads — but an operator can opt in by setting a
//! performance level, which we apply by shelling out to `amd-smi set -l`.
//!
//! Level names mirror `amd-smi set --perf-level` exactly. `determinism` is a
//! special case: `amd-smi` exposes it via `--perf-determinism <SCLKMAX>` rather
//! than `-l`, so it cannot be set through this generic path (see [`PerfLevel`]).

use std::str::FromStr;

/// A GPU performance level, matching `amd-smi set --perf-level` (`-l`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfLevel {
    Auto,
    Low,
    High,
    Manual,
    StableStd,
    StablePeak,
    StableMinMclk,
    StableMinSclk,
    Determinism,
}

impl PerfLevel {
    /// The token passed to `amd-smi set -l` for this level.
    pub fn amd_smi_arg(&self) -> &'static str {
        match self {
            PerfLevel::Auto => "auto",
            PerfLevel::Low => "low",
            PerfLevel::High => "high",
            PerfLevel::Manual => "manual",
            PerfLevel::StableStd => "stable_std",
            PerfLevel::StablePeak => "stable_peak",
            PerfLevel::StableMinMclk => "stable_min_mclk",
            PerfLevel::StableMinSclk => "stable_min_sclk",
            PerfLevel::Determinism => "determinism",
        }
    }

    /// The canonical config string for this level (same as the amd-smi token).
    pub fn as_str(&self) -> &'static str {
        self.amd_smi_arg()
    }
}

impl FromStr for PerfLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(PerfLevel::Auto),
            "low" => Ok(PerfLevel::Low),
            "high" => Ok(PerfLevel::High),
            "manual" => Ok(PerfLevel::Manual),
            "stable_std" => Ok(PerfLevel::StableStd),
            "stable_peak" => Ok(PerfLevel::StablePeak),
            "stable_min_mclk" => Ok(PerfLevel::StableMinMclk),
            "stable_min_sclk" => Ok(PerfLevel::StableMinSclk),
            "determinism" => Ok(PerfLevel::Determinism),
            other => Err(format!(
                "invalid gpu_perf_level '{other}'; expected one of: auto, low, high, manual, \
                 stable_std, stable_peak, stable_min_mclk, stable_min_sclk, determinism"
            )),
        }
    }
}

/// Apply `level` to **all** GPUs via `amd-smi set -l <level>` (no `-g`). The
/// always-on sampler sets the state before rocprofiler init — before the GPU
/// count is known. Returns `true` on success.
pub fn apply_all(level: PerfLevel) -> bool {
    if reject_determinism(level) {
        return false;
    }
    match run_amd_smi(level) {
        Ok(()) => {
            crate::debug!("gpu_amd_pmu: set all GPUs perf level to {}", level.as_str());
            true
        }
        Err(e) => {
            crate::warn!(
                "gpu_amd_pmu: failed to set perf level to {}: {e}",
                level.as_str()
            );
            false
        }
    }
}

/// `determinism` is not settable through `amd-smi -l`; warn and refuse.
fn reject_determinism(level: PerfLevel) -> bool {
    if level == PerfLevel::Determinism {
        crate::warn!(
            "gpu_perf_level=determinism cannot be set via amd-smi -l (it needs \
             --perf-determinism <SCLKMAX>); skipping"
        );
        true
    } else {
        false
    }
}

/// Run `amd-smi set -l <level>` (targets all GPUs).
fn run_amd_smi(level: PerfLevel) -> Result<(), String> {
    let output = std::process::Command::new("amd-smi")
        .args(["set", "-l", level.amd_smi_arg()])
        .output()
        .map_err(|e| format!("failed to run amd-smi (is it on PATH?): {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "amd-smi exited with {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_levels() {
        let cases = [
            ("auto", PerfLevel::Auto),
            ("LOW", PerfLevel::Low),
            ("High", PerfLevel::High),
            ("manual", PerfLevel::Manual),
            ("stable_std", PerfLevel::StableStd),
            ("stable_peak", PerfLevel::StablePeak),
            ("stable_min_mclk", PerfLevel::StableMinMclk),
            ("stable_min_sclk", PerfLevel::StableMinSclk),
            ("determinism", PerfLevel::Determinism),
        ];
        for (s, expected) in cases {
            assert_eq!(PerfLevel::from_str(s).unwrap(), expected, "parsing {s}");
        }
    }

    #[test]
    fn parse_trims_and_lowercases() {
        assert_eq!(
            PerfLevel::from_str("  Stable_Std  ").unwrap(),
            PerfLevel::StableStd
        );
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(PerfLevel::from_str("turbo").is_err());
        // The user's typo'd form (missing the trailing k) is rejected.
        assert!(PerfLevel::from_str("stable_min_mcl").is_err());
    }

    #[test]
    fn arg_roundtrips_through_parse() {
        for level in [
            PerfLevel::Auto,
            PerfLevel::StableStd,
            PerfLevel::StableMinMclk,
            PerfLevel::Determinism,
        ] {
            assert_eq!(PerfLevel::from_str(level.as_str()).unwrap(), level);
        }
    }
}
