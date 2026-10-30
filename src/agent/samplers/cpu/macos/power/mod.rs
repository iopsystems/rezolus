//! CPU and SoC energy on Apple silicon, from IOReport's `Energy Model`
//! group: running totals, in microjoules, of the energy each block has used.
//!
//! | metric | IOReport channel |
//! |---|---|
//! | `cpu_cores_energy{id="0"}` | `CPU Energy`: every CPU core |
//! | `cpu_core_energy{id}` | `EACC_CPU<n>`, `PACC<c>_CPU<n>`: one core, at its logical CPU id |
//! | `cpu_igpu_energy{id="0"}` | `GPU Energy`: the GPU |
//! | `cpu_dram_energy{id="0"}` | `DRAM` |
//! | `cpu_soc_energy{block}` | every other channel, such as `ANE`, `DISP`, `AMCC`: not a CPU cluster's (`EACC`, `PACC`), and without `CPU` or `GPU` in its name |
//!
//! The channels are counted in mJ, uJ or nJ by channel and converted. Not
//! published: the cluster totals (`EACC_CPU`, `PACC<c>_CPU`), the clusters'
//! SRAM and CPM channels, the `ECPUDTL`/`PCPUDTL` channels, and `GPU` (mJ)
//! beside `GPU Energy` (nJ). The `Energy Model` group has no package or
//! platform channel, so `cpu_package_energy` and `cpu_platform_energy` are
//! not set, nor are the C-state residencies.

const NAME: &str = "cpu_power";

use crate::agent::sampler_status::Unsupported;
use crate::agent::samplers::ioreport::{Channel, Subscription, Value};
use crate::agent::*;

use super::topology::{self, Core};

use std::sync::Mutex;

mod stats {
    include!("../../linux/power/stats.rs");
}

use stats::*;

mod soc {
    use metriken::*;

    use crate::agent::timing::AcquisitionGroup;
    use linkme::distributed_slice;

    /// Slots for SoC blocks; an M4 Max reports 14.
    pub const MAX_BLOCKS: usize = 32;

    /// The SoC blocks' energy, read in the same `Energy Model` sample as the
    /// CPU's.
    pub static CPU_POWER_SOC_ENERGY_ACQ: AcquisitionGroup =
        AcquisitionGroup::new("cpu_power", "cpu_power_soc_energy");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static CPU_POWER_SOC_ENERGY_ACQ_REG: &'static AcquisitionGroup = &CPU_POWER_SOC_ENERGY_ACQ;

    #[metric(
        name = "cpu_soc_energy",
        description = "The cumulative energy consumed by an Apple silicon SoC block other than the CPU cores, GPU and DRAM, such as the Neural Engine (ANE) or a display pipeline (DISP). Labeled with the IOReport `block` name. macOS only.",
        metadata = { unit = "microjoules", acq_group = "cpu_power_soc_energy" }
    )]
    pub static CPU_SOC_ENERGY: CounterGroup = CounterGroup::new(MAX_BLOCKS);

    pub static SOC_IDENTITY: metriken::group::SlotIdentity =
        metriken::group::SlotIdentity::grouped(SOC_IDENTITY_GROUPS);

    static SOC_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[&CPU_SOC_ENERGY]];
}

use soc::*;

const GROUP: &str = "Energy Model";

/// Where an `Energy Model` channel is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Cores,
    Core(usize),
    Gpu,
    Dram,
    /// A SoC block, by its label.
    Soc(String),
}

/// Where `name` is published, or `None` for a channel that is not.
pub fn target(name: &str, cores: &[Core]) -> Option<Target> {
    match name {
        "CPU Energy" => return Some(Target::Cores),
        "GPU Energy" => return Some(Target::Gpu),
        "DRAM" => return Some(Target::Dram),
        _ => {}
    }
    if let Some((kind, cluster, core)) = topology::parse_energy_channel(name) {
        return topology::id_of(cores, kind, cluster, core).map(Target::Core);
    }
    // The rest of the CPU clusters' channels (cluster totals, SRAM, CPM)
    // and the GPU's other one.
    if name.starts_with("EACC")
        || name.starts_with("PACC")
        || name.contains("CPU")
        || name.contains("GPU")
    {
        return None;
    }
    let block = name.strip_suffix(" Energy").unwrap_or(name);
    Some(Target::Soc(block.to_string()))
}

/// `value` in `unit` as microjoules, or `None` for a unit that is not energy
/// or a negative value.
pub fn microjoules(value: i64, unit: &str) -> Option<u64> {
    let v = u64::try_from(value).ok()?;
    match unit {
        "mJ" => v.checked_mul(1000),
        "uJ" => Some(v),
        "nJ" => Some(v / 1000),
        _ => None,
    }
}

/// Every published channel in `channels`, with its target and value.
fn read(channels: &[Channel], cores: &[Core]) -> Vec<(Target, u64)> {
    channels
        .iter()
        .filter(|c| c.group == GROUP)
        .filter_map(|c| {
            let Value::Simple(v) = c.value else {
                return None;
            };
            Some((target(&c.name, cores)?, microjoules(v, &c.unit)?))
        })
        .collect()
}

/// The groups this sampler stamps.
fn groups() -> [&'static crate::agent::timing::AcquisitionGroup; 5] {
    [
        &CPU_POWER_CORES_ENERGY_ACQ,
        &CPU_POWER_CORE_ENERGY_ACQ,
        &CPU_POWER_IGPU_ENERGY_ACQ,
        &CPU_POWER_DRAM_ENERGY_ACQ,
        &CPU_POWER_SOC_ENERGY_ACQ,
    ]
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }
    Ok(Some(Box::new(Power::new()?)))
}

impl Power {
    /// Subscribe, and set every group's members and each SoC block's slot
    /// and label from a first sample.
    fn new() -> Result<Self, anyhow::Error> {
        let cores = topology::cores()
            .ok_or_else(|| Unsupported(format!("{NAME}: no CPU topology in the device tree")))?;
        let subscription = Subscription::new(&[(GROUP, None)])
            .ok_or_else(|| Unsupported(format!("{NAME}: IOReport has no {GROUP} channels")))?;
        let channels = subscription
            .sample()
            .ok_or_else(|| anyhow::anyhow!("{NAME}: IOReport sample failed"))?;

        let read = read(&channels, &cores);
        if read.is_empty() {
            return Err(
                Unsupported(format!("{NAME}: no IOReport energy channel to publish")).into(),
            );
        }

        // Each group's members are the slots this machine reports; the groups
        // this sampler does not set declare none.
        let has = |t: &Target| read.iter().any(|(r, _)| r == t);
        let package = |present: bool| if present { vec![0] } else { vec![] };
        CPU_POWER_CORES_ENERGY_ACQ.set_member_set(&package(has(&Target::Cores)));
        CPU_POWER_IGPU_ENERGY_ACQ.set_member_set(&package(has(&Target::Gpu)));
        CPU_POWER_DRAM_ENERGY_ACQ.set_member_set(&package(has(&Target::Dram)));
        let core_ids: Vec<usize> = read
            .iter()
            .filter_map(|(t, _)| match t {
                Target::Core(id) => Some(*id),
                _ => None,
            })
            .collect();
        CPU_POWER_CORE_ENERGY_ACQ.set_member_set(&core_ids);
        for unused in [
            &CPU_POWER_PACKAGE_ENERGY_ACQ,
            &CPU_POWER_PLATFORM_ENERGY_ACQ,
            &CPU_POWER_CORE_C1_ACQ,
            &CPU_POWER_CORE_C2_ACQ,
            &CPU_POWER_CORE_C3_ACQ,
            &CPU_POWER_CORE_C6_ACQ,
            &CPU_POWER_CORE_C7_ACQ,
            &CPU_POWER_CORE_C8_ACQ,
            &CPU_POWER_CORE_C9_ACQ,
            &CPU_POWER_CORE_C10_ACQ,
            &CPU_POWER_PACKAGE_C1_ACQ,
            &CPU_POWER_PACKAGE_C2_ACQ,
            &CPU_POWER_PACKAGE_C3_ACQ,
            &CPU_POWER_PACKAGE_C6_ACQ,
            &CPU_POWER_PACKAGE_C7_ACQ,
            &CPU_POWER_PACKAGE_C8_ACQ,
            &CPU_POWER_PACKAGE_C9_ACQ,
            &CPU_POWER_PACKAGE_C10_ACQ,
            &CPU_POWER_CORE_CSTATE_ACQ,
        ] {
            unused.set_member_set(&[]);
        }

        // A slot per SoC block, labeled once: the channel list is fixed for the
        // life of the subscription.
        let mut blocks = Vec::new();
        for (t, _) in &read {
            if let Target::Soc(block) = t {
                if blocks.len() == MAX_BLOCKS {
                    warn!("{NAME}: more than {MAX_BLOCKS} SoC blocks; {block} is not published");
                    continue;
                }
                let slot = blocks.len();
                SOC_IDENTITY.assign(
                    slot,
                    std::collections::BTreeMap::from([("block".to_string(), block.clone())]),
                );
                blocks.push(block.clone());
            }
        }
        CPU_POWER_SOC_ENERGY_ACQ.set_member_set(&(0..blocks.len()).collect::<Vec<_>>());
        debug!(
            "{NAME}: {} core(s), {} SoC block(s)",
            core_ids.len(),
            blocks.len()
        );

        Ok(Power {
            subscription: Mutex::new(subscription),
            cores,
            blocks,
        })
    }
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Power {
    subscription: Mutex<Subscription>,
    cores: Vec<Core>,
    /// The SoC block at each slot.
    blocks: Vec<String>,
}

#[async_trait]
impl Sampler for Power {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // One sample supplies every group.
        let mut guards = groups().map(|g| g.acquire());
        let channels = {
            let subscription = self.subscription.lock().unwrap_or_else(|e| e.into_inner());
            subscription.sample()
        };
        let Some(channels) = channels else {
            return;
        };
        for guard in &mut guards {
            guard.mark_end();
        }
        for (target, uj) in read(&channels, &self.cores) {
            match target {
                Target::Cores => {
                    let _ = CPU_CORES_ENERGY.set(0, uj);
                }
                Target::Core(id) => {
                    let _ = CPU_CORE_ENERGY.set(id, uj);
                }
                Target::Gpu => {
                    let _ = CPU_IGPU_ENERGY.set(0, uj);
                }
                Target::Dram => {
                    let _ = CPU_DRAM_ENERGY.set(0, uj);
                }
                Target::Soc(block) => {
                    if let Some(slot) = self.blocks.iter().position(|b| *b == block) {
                        let _ = CPU_SOC_ENERGY.set(slot, uj);
                    }
                }
            }
        }
        for guard in guards {
            guard.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use topology::Kind;

    fn cores() -> Vec<Core> {
        vec![
            Core {
                id: 0,
                kind: Kind::Efficiency,
                cluster: 0,
                core: 0,
            },
            Core {
                id: 7,
                kind: Kind::Performance,
                cluster: 1,
                core: 2,
            },
        ]
    }

    #[test]
    fn channels_map_to_their_metrics() {
        let c = cores();
        assert_eq!(target("CPU Energy", &c), Some(Target::Cores));
        assert_eq!(target("GPU Energy", &c), Some(Target::Gpu));
        assert_eq!(target("DRAM", &c), Some(Target::Dram));
        assert_eq!(target("EACC_CPU0", &c), Some(Target::Core(0)));
        assert_eq!(target("PACC1_CPU2", &c), Some(Target::Core(7)));
        assert_eq!(target("ANE", &c), Some(Target::Soc("ANE".into())));
        assert_eq!(
            target("PCIe Port 0 Energy", &c),
            Some(Target::Soc("PCIe Port 0".into()))
        );
        for skipped in [
            "PACC0_CPU9",
            "EACC_CPU",
            "PACC1_CPU",
            "EACC_CPU0_SRAM",
            "PACC0_CPM",
            "ECPUDTL00",
            "GPU",
        ] {
            assert_eq!(target(skipped, &c), None, "{skipped}");
        }
    }

    #[test]
    fn units_convert_to_microjoules() {
        assert_eq!(microjoules(7, "mJ"), Some(7000));
        assert_eq!(microjoules(7, "uJ"), Some(7));
        assert_eq!(microjoules(7_400, "nJ"), Some(7));
        assert_eq!(microjoules(7, "W"), None);
        assert_eq!(microjoules(-1, "mJ"), None);
    }

    /// This machine: the CPU total, every core, the GPU, DRAM and some SoC
    /// blocks are read, and the cores add up to the CPU total within the
    /// channels' rounding.
    #[test]
    fn this_machine_is_read() {
        let cores = topology::cores().unwrap();
        let sub = Subscription::new(&[(GROUP, None)]).unwrap();
        let first = read(&sub.sample().unwrap(), &cores);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let second = read(&sub.sample().unwrap(), &cores);
        let value = |r: &[(Target, u64)], t: &Target| r.iter().find(|(x, _)| x == t).map(|x| x.1);
        for t in [Target::Cores, Target::Gpu, Target::Dram] {
            assert!(value(&second, &t).is_some(), "{t:?}");
        }
        let per_core: Vec<usize> = second
            .iter()
            .filter_map(|(t, _)| match t {
                Target::Core(id) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(per_core.len(), cores.len());
        assert!(second.iter().any(|(t, _)| matches!(t, Target::Soc(_))));

        let delta = |t: &Target| value(&second, t).unwrap() - value(&first, t).unwrap();
        let total = delta(&Target::Cores);
        let sum: u64 = per_core.iter().map(|id| delta(&Target::Core(*id))).sum();
        // Each channel rounds to 1 mJ.
        let slack = 1000 * (cores.len() as u64 + 1) + total / 20;
        assert!(
            sum.abs_diff(total) <= slack,
            "cores {sum} uJ vs total {total} uJ"
        );
    }

    /// This machine, through `new` and `refresh`: every core is a member of
    /// the per-core group, each group read is stamped, the unused groups
    /// have no members, and each SoC slot is set and labelled.
    #[tokio::test]
    async fn new_and_refresh_set_members_windows_and_blocks() {
        let power = Power::new().unwrap();
        power.refresh().await;

        let mut ids: Vec<usize> = topology::cores().unwrap().iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let mut members = CPU_POWER_CORE_ENERGY_ACQ.member_set().unwrap().to_vec();
        members.sort_unstable();
        assert_eq!(members, ids);
        for id in &ids {
            assert!(CPU_CORE_ENERGY.value(*id).is_some(), "core {id} unset");
        }
        for group in groups() {
            assert!(group.window().is_some(), "{} unstamped", group.name);
        }
        assert_eq!(CPU_POWER_PACKAGE_ENERGY_ACQ.member_set(), Some(&[][..]));
        assert_eq!(CPU_POWER_CORE_CSTATE_ACQ.member_set(), Some(&[][..]));

        assert!(!power.blocks.is_empty());
        let labels: std::collections::HashMap<usize, String> = CPU_SOC_ENERGY
            .metadata_snapshot()
            .into_iter()
            .filter_map(|(slot, m)| Some((slot, m.get("block")?.clone())))
            .collect();
        for (slot, block) in power.blocks.iter().enumerate() {
            assert_eq!(labels.get(&slot), Some(block), "slot {slot}");
            assert!(CPU_SOC_ENERGY.value(slot).is_some(), "{block} unset");
        }
    }
}
