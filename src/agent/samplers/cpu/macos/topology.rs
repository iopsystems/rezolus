//! Apple silicon CPU topology from the device tree, for mapping IOReport's
//! per-core channels onto the logical CPU ids `cpu_usage` reports, and each
//! cluster type's performance-state frequency table.

use crate::agent::samplers::iokit::{CfString, Service};

/// A core's cluster type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Efficiency,
    Performance,
}

impl Kind {
    fn from_letter(c: char) -> Option<Kind> {
        match c {
            'E' => Some(Kind::Efficiency),
            'P' => Some(Kind::Performance),
            _ => None,
        }
    }
}

/// One core: its logical CPU id, and its position as IOReport names it, the
/// index of its cluster among the clusters of its kind and its index within
/// the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Core {
    pub id: usize,
    pub kind: Kind,
    pub cluster: usize,
    pub core: usize,
}

/// A core as the device tree describes it: `cluster-id` is global across
/// kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub id: usize,
    pub kind: Kind,
    pub cluster_id: u32,
    pub core: usize,
}

/// Number each core's cluster among the clusters of its kind, in
/// `cluster-id` order.
pub fn cores_from_nodes(nodes: &[Node]) -> Vec<Core> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut ids: Vec<u32> = nodes
            .iter()
            .filter(|n| n.kind == node.kind)
            .map(|n| n.cluster_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let cluster = ids.iter().position(|c| *c == node.cluster_id).unwrap_or(0);
        out.push(Core {
            id: node.id,
            kind: node.kind,
            cluster,
            core: node.core,
        });
    }
    out
}

fn le_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// Every core under `IODeviceTree:/cpus`. `None` when the device tree has
/// no such node or a core lacks a property.
pub fn cores() -> Option<Vec<Core>> {
    let cpus = Service::from_path("IODeviceTree:/cpus")?;
    let kind_key = CfString::new("cluster-type")?;
    let cluster_key = CfString::new("cluster-id")?;
    let core_key = CfString::new("cluster-core-id")?;
    let id_key = CfString::new("logical-cpu-id")?;
    let mut nodes = Vec::new();
    for cpu in cpus.device_tree_children()? {
        let kind = Kind::from_letter(*cpu.data(&kind_key)?.first()? as char)?;
        nodes.push(Node {
            id: usize::try_from(cpu.number(&id_key)?).ok()?,
            kind,
            cluster_id: le_u32(&cpu.data(&cluster_key)?)?,
            core: le_u32(&cpu.data(&core_key)?)? as usize,
        });
    }
    (!nodes.is_empty()).then(|| cores_from_nodes(&nodes))
}

/// The logical CPU id of the core at `(kind, cluster, core)`.
pub fn id_of(cores: &[Core], kind: Kind, cluster: usize, core: usize) -> Option<usize> {
    cores
        .iter()
        .find(|c| c.kind == kind && c.cluster == cluster && c.core == core)
        .map(|c| c.id)
}

/// The position named by a performance-state channel: `ECPU010` is the E
/// cluster 0, core 1; `PCPU150` the P cluster 1, core 5. A one-digit suffix
/// is a core of cluster 0, a two-digit one is cluster and core, and a
/// three-digit one is cluster, core and `0`. A third digit other than `0`
/// is not mapped, since its meaning is unknown.
pub fn parse_state_channel(name: &str) -> Option<(Kind, usize, usize)> {
    let kind = Kind::from_letter(name.chars().next()?)?;
    let digits = name.get(1..)?.strip_prefix("CPU")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let d: Vec<usize> = digits.bytes().map(|b| (b - b'0') as usize).collect();
    match d.as_slice() {
        [core] => Some((kind, 0, *core)),
        [cluster, core] | [cluster, core, 0] => Some((kind, *cluster, *core)),
        _ => None,
    }
}

/// The position named by a per-core energy channel: `EACC_CPU2` is the E
/// cluster 0, core 2; `PACC1_CPU5` the P cluster 1, core 5.
pub fn parse_energy_channel(name: &str) -> Option<(Kind, usize, usize)> {
    let (cluster, core) = name.split_once("_CPU")?;
    let kind = Kind::from_letter(cluster.chars().next()?)?;
    let cluster = cluster.get(1..)?.strip_prefix("ACC")?;
    let cluster = if cluster.is_empty() {
        0
    } else {
        cluster.parse().ok()?
    };
    Some((kind, cluster, core.parse().ok()?))
}

/// The index `k` of a performance state named `V<k>P<n>`; `None` for
/// `IDLE`, `DOWN` and anything else.
pub fn state_index(name: &str) -> Option<usize> {
    let rest = name.strip_prefix('V')?;
    let (k, n) = rest.split_once('P')?;
    n.parse::<u32>().ok()?;
    k.parse().ok()
}

/// A frequency table property's frequencies in Hz: pairs of little-endian
/// `u32` frequency and voltage, the frequency in Hz or kHz.
pub fn parse_frequency_table(bytes: &[u8]) -> Vec<u64> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&[a, b, c, d, ..]| {
            let f = u64::from(u32::from_le_bytes([a, b, c, d]));
            // No performance state runs below 100 MHz, so a value under
            // 10^8 is in kHz.
            if f < 100_000_000 {
                f * 1000
            } else {
                f
            }
        })
        .collect()
}

/// The E and P clusters' frequency tables, in Hz, from the power manager's
/// `voltage-states1-sram` and `voltage-states5-sram`.
pub fn frequency_tables() -> Option<(Vec<u64>, Vec<u64>)> {
    let pmgr = Service::from_path("IODeviceTree:/arm-io/pmgr")?;
    let table = |key: &str| -> Option<Vec<u64>> {
        Some(parse_frequency_table(&pmgr.data(&CfString::new(key)?)?))
    };
    Some((
        table("voltage-states1-sram")?,
        table("voltage-states5-sram")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_are_numbered_within_their_kind() {
        let node = |id, kind, cluster_id, core| Node {
            id,
            kind,
            cluster_id,
            core,
        };
        let cores = cores_from_nodes(&[
            node(0, Kind::Efficiency, 0, 0),
            node(1, Kind::Efficiency, 0, 1),
            node(2, Kind::Performance, 1, 0),
            node(3, Kind::Performance, 2, 0),
            node(4, Kind::Performance, 2, 1),
        ]);
        assert_eq!(id_of(&cores, Kind::Efficiency, 0, 1), Some(1));
        assert_eq!(id_of(&cores, Kind::Performance, 0, 0), Some(2));
        assert_eq!(id_of(&cores, Kind::Performance, 1, 1), Some(4));
        assert_eq!(id_of(&cores, Kind::Performance, 2, 0), None);
    }

    #[test]
    fn channel_names_parse() {
        assert_eq!(
            parse_state_channel("ECPU010"),
            Some((Kind::Efficiency, 0, 1))
        );
        assert_eq!(
            parse_state_channel("PCPU150"),
            Some((Kind::Performance, 1, 5))
        );
        assert_eq!(
            parse_state_channel("PCPU3"),
            Some((Kind::Performance, 0, 3))
        );
        assert_eq!(parse_state_channel("ECPU"), None);
        assert_eq!(parse_state_channel("PCPU151"), None);
        assert_eq!(parse_state_channel("GPUPH"), None);
        assert_eq!(
            parse_energy_channel("EACC_CPU2"),
            Some((Kind::Efficiency, 0, 2))
        );
        assert_eq!(
            parse_energy_channel("PACC1_CPU5"),
            Some((Kind::Performance, 1, 5))
        );
        assert_eq!(parse_energy_channel("PACC1_CPU5_SRAM"), None);
        assert_eq!(parse_energy_channel("PACC0_CPM"), None);
        assert_eq!(parse_energy_channel("CPU Energy"), None);
        assert_eq!(state_index("V12P7"), Some(12));
        assert_eq!(state_index("IDLE"), None);
        assert_eq!(state_index("DOWN"), None);
    }

    #[test]
    fn frequency_tables_in_khz_and_hz_read_as_hz() {
        let pair = |f: u32| [f.to_le_bytes(), 800u32.to_le_bytes()].concat();
        let khz = [pair(1_020_000), pair(4_512_000)].concat();
        assert_eq!(parse_frequency_table(&khz), [1_020_000_000, 4_512_000_000]);
        let hz = [pair(600_000_000), pair(2_064_000_000)].concat();
        assert_eq!(parse_frequency_table(&hz), [600_000_000, 2_064_000_000]);
    }

    /// This machine's device tree: every core is mapped, ids are unique, and
    /// each table rises.
    #[test]
    fn the_device_tree_is_read() {
        let cores = cores().expect("cores");
        let mut ids: Vec<usize> = cores.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cores.len());
        assert_eq!(
            ids.len(),
            std::thread::available_parallelism().unwrap().get()
        );
        let (e, p) = frequency_tables().expect("tables");
        for table in [&e, &p] {
            assert!(!table.is_empty());
            assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
            assert!(table[0] >= 100_000_000 && *table.last().unwrap() < 10_000_000_000);
        }
    }
}
