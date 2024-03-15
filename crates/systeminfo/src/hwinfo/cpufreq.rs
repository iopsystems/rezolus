use super::util::*;
use crate::Result;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cpufreq {
    pub affected_cpus: Vec<usize>,
    pub related_cpus: Vec<usize>,
    pub freqdomain_cpus: Vec<usize>,
    pub cpuinfo_max_freq: usize,
    pub cpuinfo_min_freq: usize,
    // intel_pstate driver doesn't expose cpuinfo_cur_freq
    pub cpuinfo_cur_freq: Option<usize>,
    pub cpuinfo_transition_latency: usize,
    pub scaling_driver: String,
    pub scaling_governor: String,
    pub scaling_available_frequencies: Vec<usize>,
    pub scaling_available_governors: Vec<String>,
    pub scaling_min_freq: usize,
    pub scaling_max_freq: usize,
    pub scaling_cur_freq: usize,
}

impl Cpufreq {
    pub fn new(cpu: usize) -> Result<Self> {
        // related CPUs
        let affected_cpus: Vec<usize> = read_space_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/affected_cpus"
        ))?;
        let related_cpus = read_space_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/related_cpus"
        ))?;
        let freqdomain_cpus = read_space_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/freqdomain_cpus"
        ))?;
        // cpuinfo
        let cpuinfo_min_freq = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_min_freq"
        ))?;
        let cpuinfo_max_freq = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq"
        ))?;
        let cpuinfo_cur_freq: Option<usize> = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_cur_freq"
        ))
        .map_or(None, |v| Some(v));
        let cpuinfo_transition_latency = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_transition_latency"
        ))?;
        // scaling
        let scaling_driver = read_string(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_driver"
        ))?;
        let scaling_governor = read_string(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor"
        ))?;
        let scaling_available_frequencies = read_space_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_available_frequencies"
        ))?;
        let scaling_available_governors = read_space_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_available_governors"
        ))?;
        let scaling_min_freq = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_min_freq"
        ))?;
        let scaling_max_freq = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_max_freq"
        ))?;
        let scaling_cur_freq = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"
        ))?;
        Ok(Cpufreq {
            affected_cpus,
            related_cpus,
            freqdomain_cpus,
            cpuinfo_max_freq,
            cpuinfo_min_freq,
            cpuinfo_cur_freq,
            cpuinfo_transition_latency,
            scaling_driver,
            scaling_governor,
            scaling_available_frequencies,
            scaling_available_governors,
            scaling_min_freq,
            scaling_max_freq,
            scaling_cur_freq,
        })
    }
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CpuFreqBoosting {
    Enabled,
    Disabled,
    Unknown,
}

// Check whether the CPU frequency turbo/boosting is enabled or not, report Unknown if
// the CPU frequency scaling driver doesn't expose the status
pub fn get_cpu_boosting() -> CpuFreqBoosting {
    if let Ok(no_turbo) = read_usize("/sys/devices/system/cpu/intel_pstate/no_turbo") {
        if no_turbo == 0 {
            CpuFreqBoosting::Enabled
        } else {
            CpuFreqBoosting::Disabled
        }
    } else if let Ok(boosting) = read_usize("/sys/devices/system/cpu/cpufreq/boost") {
        if boosting == 1 {
            CpuFreqBoosting::Enabled
        } else {
            CpuFreqBoosting::Disabled
        }
    } else {
        CpuFreqBoosting::Unknown
    }
}
