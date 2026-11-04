//! Apple silicon CPU topology from the device tree, for mapping IOReport's
//! per-core channels onto the logical CPU ids `cpu_usage` reports, and the
//! power manager's performance-state frequency tables, which `gpu_apple`
//! also reads.

use crate::agent::samplers::iokit::{CfString, Service};

use std::collections::HashMap;

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

/// The power manager's frequency table `key`, in Hz.
pub fn frequency_table(key: &str) -> Option<Vec<u64>> {
    let pmgr = Service::from_path("IODeviceTree:/arm-io/pmgr")?;
    Some(parse_frequency_table(&pmgr.data(&CfString::new(key)?)?))
}

/// The size of a `perf-domains` entry.
const PERF_DOMAIN_LEN: usize = 28;

/// The CPU clusters in the power manager's `perf-domains`, each with the
/// index `n` of its `voltage-states<n>` tables. An entry is 28 bytes: the
/// index in byte 0 and a NUL-padded name in the last 16, `ECPU`, `PCPU`,
/// `PCPU1`, ... for the CPU clusters (cluster 0 has no number). Entries with
/// other names (`SOC`, `ANE`, `DISP`, ...) are skipped, and a property whose
/// length is not a multiple of 28 gives none.
pub fn parse_perf_domains(bytes: &[u8]) -> Vec<((Kind, usize), u8)> {
    if !bytes.len().is_multiple_of(PERF_DOMAIN_LEN) {
        return Vec::new();
    }
    bytes
        .as_chunks::<PERF_DOMAIN_LEN>()
        .0
        .iter()
        .filter_map(|entry| {
            let name = &entry[PERF_DOMAIN_LEN - 16..];
            let name = std::str::from_utf8(name.split(|b| *b == 0).next()?).ok()?;
            let kind = Kind::from_letter(name.chars().next()?)?;
            let digits = name.get(1..)?.strip_prefix("CPU")?;
            let cluster = if digits.is_empty() {
                0
            } else {
                digits.parse().ok()?
            };
            Some(((kind, cluster), entry[0]))
        })
        .collect()
}

/// Each CPU cluster's performance-state frequencies, in Hz, keyed by kind
/// and the cluster's index among the clusters of its kind, and the clusters
/// `perf-domains` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrequencyTables {
    pub tables: HashMap<(Kind, usize), Vec<u64>>,
    pub listed: Vec<(Kind, usize)>,
}

impl FrequencyTables {
    /// The table of cluster `cluster` of `kind`. A cluster `perf-domains`
    /// does not list reads cluster 0's table of its kind; a listed cluster
    /// reads only its own.
    pub fn get(&self, kind: Kind, cluster: usize) -> Option<&[u64]> {
        let own = self.tables.get(&(kind, cluster));
        if self.listed.contains(&(kind, cluster)) {
            return own.map(Vec::as_slice);
        }
        own.or_else(|| self.tables.get(&(kind, 0)))
            .map(Vec::as_slice)
    }
}

/// The tables `table` finds for the CPU clusters `domains` lists:
/// `voltage-states<n>-sram` for each listed cluster, and for cluster 0 of a
/// kind `domains` does not list, `voltage-states1-sram` (E) or
/// `voltage-states5-sram` (P). A listed cluster whose table is missing gets
/// none, and [`FrequencyTables::get`] gives it none.
pub fn tables_from(
    domains: &[((Kind, usize), u8)],
    table: impl Fn(&str) -> Option<Vec<u64>>,
) -> FrequencyTables {
    let mut tables = HashMap::new();
    for (cluster, n) in domains {
        if let Some(t) = table(&format!("voltage-states{n}-sram")) {
            tables.insert(*cluster, t);
        }
    }
    for (kind, key) in [
        (Kind::Efficiency, "voltage-states1-sram"),
        (Kind::Performance, "voltage-states5-sram"),
    ] {
        if domains.iter().any(|((k, _), _)| *k == kind) {
            continue;
        }
        if let Some(t) = table(key) {
            tables.insert((kind, 0), t);
        }
    }
    FrequencyTables {
        tables,
        listed: domains.iter().map(|(cluster, _)| *cluster).collect(),
    }
}

/// The CPU clusters' frequency tables from the power manager's
/// `perf-domains` and `voltage-states` properties (see [`tables_from`]).
/// `None` when no table is found.
pub fn frequency_tables() -> Option<FrequencyTables> {
    let pmgr = Service::from_path("IODeviceTree:/arm-io/pmgr")?;
    let domains = CfString::new("perf-domains")
        .and_then(|key| pmgr.data(&key))
        .map(|bytes| parse_perf_domains(&bytes))
        .unwrap_or_default();
    let tables = tables_from(&domains, |key| {
        Some(parse_frequency_table(&pmgr.data(&CfString::new(key)?)?))
    });
    (!tables.tables.is_empty()).then_some(tables)
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

    /// This M4 Max's `perf-domains`: SOC, ECPU, DCS, PCPU, ANE, SOC_AFC,
    /// SOC_AFI, DISP and PCPU1.
    const M4_MAX_PERF_DOMAINS: &str = "00040001000000002c010000534f43000000000000000000000000000101010200000000000000004543505500000000000000000000000002040003000000002c0100004443530000000000000000000000000005010105000000000000000050435055000000000000000000000000080000080000000000000000414e45000000000000000000000000001c00000f0000000000000000534f435f4146430000000000000000001d0000100000000000000000534f435f4146490000000000000000000b04000b000000002c010000444953500000000000000000000000000d01010d000000000000000050435055310000000000000000000000";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn perf_domains_name_the_cpu_clusters_tables() {
        let bytes = unhex(M4_MAX_PERF_DOMAINS);
        assert_eq!(
            parse_perf_domains(&bytes),
            [
                ((Kind::Efficiency, 0), 1),
                ((Kind::Performance, 0), 5),
                ((Kind::Performance, 1), 13),
            ]
        );
        assert_eq!(parse_perf_domains(&bytes[1..]), []);
    }

    #[test]
    fn listed_clusters_read_their_tables_and_unlisted_kinds_the_defaults() {
        let present = |keys: &'static [(&'static str, u64)]| {
            move |key: &str| keys.iter().find(|(k, _)| *k == key).map(|(_, f)| vec![*f])
        };
        let e = (Kind::Efficiency, 0);
        let p0 = (Kind::Performance, 0);
        let p1 = (Kind::Performance, 1);
        let defaults: &[(&str, u64)] = &[
            ("voltage-states1-sram", 1),
            ("voltage-states5-sram", 5),
            ("voltage-states13-sram", 13),
        ];

        // Every cluster listed: each its own table.
        let tables = tables_from(&[(e, 1), (p0, 5), (p1, 13)], present(defaults));
        assert_eq!(
            tables.tables,
            HashMap::from([(e, vec![1]), (p0, vec![5]), (p1, vec![13])])
        );

        // No perf-domains: cluster 0 of each kind reads the default.
        let tables = tables_from(&[], present(defaults));
        assert_eq!(tables.tables, HashMap::from([(e, vec![1]), (p0, vec![5])]));

        // P listed, E not: E reads the default, P only its listed table.
        let tables = tables_from(
            &[(p0, 9)],
            present(&[("voltage-states1-sram", 1), ("voltage-states9-sram", 9)]),
        );
        assert_eq!(tables.tables, HashMap::from([(e, vec![1]), (p0, vec![9])]));

        // A listed kind whose table is missing does not read the default.
        let tables = tables_from(&[(e, 1), (p0, 9)], present(defaults));
        assert_eq!(tables.tables, HashMap::from([(e, vec![1])]));
    }

    #[test]
    fn an_unlisted_cluster_reads_cluster_0s_table() {
        let mut tables = FrequencyTables {
            tables: HashMap::from([
                ((Kind::Performance, 0), vec![1]),
                ((Kind::Performance, 1), vec![2]),
            ]),
            listed: Vec::new(),
        };
        assert_eq!(tables.get(Kind::Performance, 1), Some(&[2][..]));
        assert_eq!(tables.get(Kind::Performance, 2), Some(&[1][..]));
        assert_eq!(tables.get(Kind::Efficiency, 0), None);
        // A listed cluster without a table reads none.
        tables.listed.push((Kind::Performance, 2));
        assert_eq!(tables.get(Kind::Performance, 2), None);
        assert_eq!(tables.get(Kind::Performance, 3), Some(&[1][..]));
    }

    /// This machine's device tree: every core is mapped, ids are unique, and
    /// each table rises.
    #[test]
    fn the_device_tree_is_read() {
        crate::agent::samplers::ioreport::skip_in_vm!();
        let cores = cores().expect("cores");
        let mut ids: Vec<usize> = cores.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), cores.len());
        assert_eq!(
            ids.len(),
            std::thread::available_parallelism().unwrap().get()
        );
        let tables = frequency_tables().expect("tables");
        // Every core has a table, and every cluster perf-domains lists has
        // its own.
        let pmgr = Service::from_path("IODeviceTree:/arm-io/pmgr").unwrap();
        let domains = CfString::new("perf-domains")
            .and_then(|key| pmgr.data(&key))
            .map(|bytes| parse_perf_domains(&bytes))
            .unwrap_or_default();
        for c in &cores {
            assert!(tables.get(c.kind, c.cluster).is_some(), "{c:?}");
            if domains.iter().any(|(d, _)| *d == (c.kind, c.cluster)) {
                assert!(tables.tables.contains_key(&(c.kind, c.cluster)), "{c:?}");
            }
        }
        for table in tables.tables.values() {
            assert!(!table.is_empty());
            assert!(table.windows(2).all(|w| w[0] < w[1]), "{table:?}");
            assert!(table[0] >= 100_000_000 && *table.last().unwrap() < 10_000_000_000);
        }
    }
}
